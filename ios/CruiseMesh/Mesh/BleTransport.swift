import CoreBluetooth
import Foundation
import os.log

/// Dual-role BLE GATT transport (DESIGN.md §5.2) — central scan/connect + peripheral advertise.
final class BleTransport: NSObject {
    typealias FrameHandler = (String, Data) -> Void
    typealias ConnectionHandler = (String) -> Void

    private let log = Logger(subsystem: "com.cruisemesh", category: "BleTransport")
    private var central: CBCentralManager!
    private var peripheralManager: CBPeripheralManager!

    private var inboundChar: CBMutableCharacteristic?
    private var outboundChar: CBMutableCharacteristic?

    private var peripheralsById: [String: CBPeripheral] = [:]
    private var writeChars: [String: CBCharacteristic] = [:]
    private var notifyChars: [String: CBCharacteristic] = [:]
    private var reassemblers: [String: FrameReassembler] = [:]
    private var mtuPayload: [String: Int] = [:]
    private var subscribedCentrals: [String: CBCentral] = [:]
    private let backoff = ReconnectBackoffTracker()

    var onFrame: FrameHandler?
    var onCentralConnected: ConnectionHandler?
    var onCentralDisconnected: ConnectionHandler?
    var onPeripheralSubscribed: ConnectionHandler?
    var onPeripheralUnsubscribed: ConnectionHandler?

    private var running = false

    override init() {
        super.init()
        central = CBCentralManager(delegate: self, queue: .global(qos: .userInitiated))
        peripheralManager = CBPeripheralManager(delegate: self, queue: .global(qos: .userInitiated))
    }

    func start() {
        running = true
        startScanIfReady()
        startAdvertisingIfReady()
    }

    func stop() {
        running = false
        if central.state == .poweredOn {
            central.stopScan()
            for p in peripheralsById.values {
                central.cancelPeripheralConnection(p)
            }
        }
        if peripheralManager.state == .poweredOn {
            peripheralManager.stopAdvertising()
            peripheralManager.removeAllServices()
        }
        peripheralsById.removeAll()
        writeChars.removeAll()
        notifyChars.removeAll()
        reassemblers.removeAll()
        subscribedCentrals.removeAll()
    }

    func sendAsCentral(address: String, frame: Data) {
        guard let peripheral = peripheralsById[address],
              let characteristic = writeChars[address] else { return }
        let payloadSize = mtuPayload[address] ?? (FrameFraming.defaultAttMtu - FrameFraming.attHeaderOverhead)
        for fragment in FrameFraming.fragment(frame: frame, mtuPayloadSize: payloadSize) {
            peripheral.writeValue(fragment, for: characteristic, type: .withoutResponse)
        }
    }

    func sendAsPeripheral(address: String, frame: Data) {
        guard let central = subscribedCentrals[address],
              let characteristic = outboundChar else { return }
        let payloadSize = mtuPayload[address] ?? (FrameFraming.defaultAttMtu - FrameFraming.attHeaderOverhead)
        for fragment in FrameFraming.fragment(frame: frame, mtuPayloadSize: payloadSize) {
            peripheralManager.updateValue(fragment, for: characteristic, onSubscribedCentrals: [central])
        }
    }

    private func startScanIfReady() {
        guard running, central.state == .poweredOn else { return }
        central.scanForPeripherals(
            withServices: [MeshConstants.serviceUUID],
            options: [CBCentralManagerScanOptionAllowDuplicatesKey: false]
        )
        log.info("Central scanning for CruiseMesh service")
    }

    private func startAdvertisingIfReady() {
        guard running, peripheralManager.state == .poweredOn else { return }
        let inbound = CBMutableCharacteristic(
            type: MeshConstants.inboundCharacteristicUUID,
            properties: [.write, .writeWithoutResponse],
            value: nil,
            permissions: [.writeable]
        )
        let outbound = CBMutableCharacteristic(
            type: MeshConstants.outboundCharacteristicUUID,
            properties: [.notify, .read],
            value: nil,
            permissions: [.readable]
        )
        inboundChar = inbound
        outboundChar = outbound
        let service = CBMutableService(type: MeshConstants.serviceUUID, primary: true)
        service.characteristics = [inbound, outbound]
        peripheralManager.removeAllServices()
        peripheralManager.add(service)
        peripheralManager.startAdvertising([
            CBAdvertisementDataServiceUUIDsKey: [MeshConstants.serviceUUID],
            CBAdvertisementDataLocalNameKey: "CruiseMesh",
        ])
        log.info("Peripheral advertising CruiseMesh service")
    }

    private func handleFragment(address: String, fragment: Data) {
        let reassembler = reassemblers[address] ?? FrameReassembler()
        reassemblers[address] = reassembler
        if let frame = reassembler.accept(fragment) {
            onFrame?(address, frame)
        }
    }
}

extension BleTransport: CBCentralManagerDelegate {
    func centralManagerDidUpdateState(_ central: CBCentralManager) {
        if central.state == .poweredOn { startScanIfReady() }
    }

    func centralManager(
        _ central: CBCentralManager,
        didDiscover peripheral: CBPeripheral,
        advertisementData: [String: Any],
        rssi RSSI: NSNumber
    ) {
        let address = peripheral.identifier.uuidString
        // Self-connection guard: skip our own instance token if present in manufacturer/service data.
        if let serviceData = advertisementData[CBAdvertisementDataServiceDataKey] as? [CBUUID: Data],
           let token = serviceData[MeshConstants.serviceUUID],
           token == MeshConstants.localInstanceId {
            return
        }
        let now = Int64(Date().timeIntervalSince1970 * 1000)
        guard backoff.canAttempt(address: address, nowMs: now) else { return }
        if peripheralsById[address] != nil { return }
        peripheralsById[address] = peripheral
        peripheral.delegate = self
        central.connect(peripheral, options: nil)
    }

    func centralManager(_ central: CBCentralManager, didConnect peripheral: CBPeripheral) {
        let address = peripheral.identifier.uuidString
        backoff.recordSuccess(address: address)
        reassemblers[address] = FrameReassembler()
        peripheral.discoverServices([MeshConstants.serviceUUID])
    }

    func centralManager(_ central: CBCentralManager, didFailToConnect peripheral: CBPeripheral, error: Error?) {
        let address = peripheral.identifier.uuidString
        let now = Int64(Date().timeIntervalSince1970 * 1000)
        backoff.recordFailure(address: address, nowMs: now)
        peripheralsById.removeValue(forKey: address)
    }

    func centralManager(_ central: CBCentralManager, didDisconnectPeripheral peripheral: CBPeripheral, error: Error?) {
        let address = peripheral.identifier.uuidString
        let now = Int64(Date().timeIntervalSince1970 * 1000)
        backoff.recordFailure(address: address, nowMs: now)
        peripheralsById.removeValue(forKey: address)
        writeChars.removeValue(forKey: address)
        notifyChars.removeValue(forKey: address)
        reassemblers.removeValue(forKey: address)
        onCentralDisconnected?(address)
    }
}

extension BleTransport: CBPeripheralDelegate {
    func peripheral(_ peripheral: CBPeripheral, didDiscoverServices error: Error?) {
        guard let services = peripheral.services else { return }
        for service in services where service.uuid == MeshConstants.serviceUUID {
            peripheral.discoverCharacteristics(
                [MeshConstants.inboundCharacteristicUUID, MeshConstants.outboundCharacteristicUUID],
                for: service
            )
        }
    }

    func peripheral(_ peripheral: CBPeripheral, didDiscoverCharacteristicsFor service: CBService, error: Error?) {
        let address = peripheral.identifier.uuidString
        guard let characteristics = service.characteristics else { return }
        for c in characteristics {
            if c.uuid == MeshConstants.inboundCharacteristicUUID {
                writeChars[address] = c
            } else if c.uuid == MeshConstants.outboundCharacteristicUUID {
                notifyChars[address] = c
                peripheral.setNotifyValue(true, for: c)
            }
        }
        // iOS doesn't expose negotiated ATT MTU the same way; use a conservative large value.
        mtuPayload[address] = 512 - FrameFraming.attHeaderOverhead
        if writeChars[address] != nil, notifyChars[address] != nil {
            onCentralConnected?(address)
        }
    }

    func peripheral(_ peripheral: CBPeripheral, didUpdateValueFor characteristic: CBCharacteristic, error: Error?) {
        guard let value = characteristic.value else { return }
        handleFragment(address: peripheral.identifier.uuidString, fragment: value)
    }
}

extension BleTransport: CBPeripheralManagerDelegate {
    func peripheralManagerDidUpdateState(_ peripheral: CBPeripheralManager) {
        if peripheral.state == .poweredOn { startAdvertisingIfReady() }
    }

    func peripheralManager(
        _ peripheral: CBPeripheralManager,
        central: CBCentral,
        didSubscribeTo characteristic: CBCharacteristic
    ) {
        let address = central.identifier.uuidString
        subscribedCentrals[address] = central
        reassemblers[address] = FrameReassembler()
        mtuPayload[address] = max(20, central.maximumUpdateValueLength)
        onPeripheralSubscribed?(address)
    }

    func peripheralManager(
        _ peripheral: CBPeripheralManager,
        central: CBCentral,
        didUnsubscribeFrom characteristic: CBCharacteristic
    ) {
        let address = central.identifier.uuidString
        subscribedCentrals.removeValue(forKey: address)
        reassemblers.removeValue(forKey: address)
        onPeripheralUnsubscribed?(address)
    }

    func peripheralManager(
        _ peripheral: CBPeripheralManager,
        didReceiveWrite requests: [CBATTRequest]
    ) {
        for request in requests {
            let address = request.central.identifier.uuidString
            if let value = request.value {
                handleFragment(address: address, fragment: value)
            }
            peripheral.respond(to: request, withResult: .success)
        }
    }
}
