import CoreBluetooth
import Foundation
import os.log

/// Dual-role BLE GATT transport (DESIGN.md §5.2) — central scan/connect + peripheral advertise.
final class BleTransport: NSObject {
    typealias FrameHandler = (String, Data) -> Void
    typealias ConnectionHandler = (String) -> Void

    private let log = Logger(subsystem: "com.cruisemesh", category: "BleTransport")
    private let queue = DispatchQueue(label: "com.cruisemesh.ble", qos: .userInitiated)
    private var central: CBCentralManager!
    private var peripheralManager: CBPeripheralManager!

    private var inboundChar: CBMutableCharacteristic?
    private var outboundChar: CBMutableCharacteristic?

    private var peripheralsById: [String: CBPeripheral] = [:]
    private var writeChars: [String: CBCharacteristic] = [:]
    private var notifyChars: [String: CBCharacteristic] = [:]
    private var reassemblers: [String: FrameReassembler] = [:]
    private var centralMtuPayload: [String: Int] = [:]
    private var peripheralMtuPayload: [String: Int] = [:]
    private var subscribedCentrals: [String: CBCentral] = [:]
    private var pendingCentralWrites: [String: [Data]] = [:]
    private var pendingPeripheralUpdates: [String: [Data]] = [:]
    private var readyCentralAddresses = Set<String>()
    private var reconnectWorkItems: [String: DispatchWorkItem] = [:]
    private let backoff = ReconnectBackoffTracker()

    var onFrame: FrameHandler?
    var onCentralConnected: ConnectionHandler?
    var onCentralDisconnected: ConnectionHandler?
    var onPeripheralSubscribed: ConnectionHandler?
    var onPeripheralUnsubscribed: ConnectionHandler?

    private var running = false

    override init() {
        super.init()
        // FI3: restoration identifiers let iOS relaunch the app in the
        // background on a BLE event after jetsam/termination and redeliver
        // this manager's prior state via `willRestoreState` below, instead
        // of the mesh silently staying dead until the user reopens the app.
        // `CruiseMeshApp`'s `AppDelegate.application(_:didFinishLaunchingWithOptions:)`
        // touches `MeshController.shared` (and therefore this initializer)
        // as early as possible on such a relaunch so these managers exist
        // in time for the system to deliver the restore callback.
        central = CBCentralManager(
            delegate: self,
            queue: queue,
            options: [CBCentralManagerOptionRestoreIdentifierKey: MeshConstants.bleCentralRestoreIdentifier]
        )
        peripheralManager = CBPeripheralManager(
            delegate: self,
            queue: queue,
            options: [CBPeripheralManagerOptionRestoreIdentifierKey: MeshConstants.blePeripheralRestoreIdentifier]
        )
    }

    func start() {
        queue.async { [weak self] in
            guard let self else { return }
            self.running = true
            self.startScanIfReady()
            self.startAdvertisingIfReady()
        }
    }

    func stop() {
        queue.async { [weak self] in
            guard let self else { return }
            self.running = false
            if self.central.state == .poweredOn {
                self.central.stopScan()
                for peripheral in self.peripheralsById.values {
                    self.central.cancelPeripheralConnection(peripheral)
                }
            }
            if self.peripheralManager.state == .poweredOn {
                self.peripheralManager.stopAdvertising()
                self.peripheralManager.removeAllServices()
            }
            // FI3: `startAdvertisingIfReady` now treats a non-nil
            // inboundChar/outboundChar as "the service is still registered
            // with the peripheral manager, just (re)start advertising" --
            // true after a restore, but NOT true here, since
            // `removeAllServices()` just unregistered it. Clear both so the
            // next `start()` rebuilds the service instead of advertising
            // with nothing behind it.
            self.inboundChar = nil
            self.outboundChar = nil
            self.peripheralsById.removeAll()
            self.writeChars.removeAll()
            self.notifyChars.removeAll()
            self.reassemblers.removeAll()
            self.centralMtuPayload.removeAll()
            self.peripheralMtuPayload.removeAll()
            self.subscribedCentrals.removeAll()
            self.pendingCentralWrites.removeAll()
            self.pendingPeripheralUpdates.removeAll()
            self.readyCentralAddresses.removeAll()
            for workItem in self.reconnectWorkItems.values {
                workItem.cancel()
            }
            self.reconnectWorkItems.removeAll()
            self.backoff.clear()
        }
    }

    func sendAsCentral(address: String, frame: Data) {
        queue.async { [weak self] in
            guard let self,
                  let peripheral = self.peripheralsById[address],
                  self.writeChars[address] != nil else { return }
            let payloadSize = self.centralMtuPayload[address]
                ?? peripheral.maximumWriteValueLength(for: .withoutResponse)
            let fragments = FrameFraming.fragment(frame: frame, mtuPayloadSize: payloadSize)
            guard !fragments.isEmpty else {
                self.log.error("Frame too large for BLE fragmentation to \(address, privacy: .public)")
                return
            }
            self.pendingCentralWrites[address, default: []].append(contentsOf: fragments)
            self.flushCentralWrites(address: address)
        }
    }

    func sendAsPeripheral(address: String, frame: Data) {
        queue.async { [weak self] in
            guard let self, self.subscribedCentrals[address] != nil, self.outboundChar != nil else { return }
            let payloadSize = self.peripheralMtuPayload[address]
                ?? (FrameFraming.defaultAttMtu - FrameFraming.attHeaderOverhead)
            let fragments = FrameFraming.fragment(frame: frame, mtuPayloadSize: payloadSize)
            guard !fragments.isEmpty else {
                self.log.error("Frame too large for BLE fragmentation to \(address, privacy: .public)")
                return
            }
            self.pendingPeripheralUpdates[address, default: []].append(contentsOf: fragments)
            self.flushPeripheralUpdates(address: address)
        }
    }

    private func flushCentralWrites(address: String) {
        guard let peripheral = peripheralsById[address],
              let characteristic = writeChars[address] else {
            pendingCentralWrites.removeValue(forKey: address)
            return
        }
        while peripheral.canSendWriteWithoutResponse,
              var pending = pendingCentralWrites[address],
              !pending.isEmpty {
            let fragment = pending.removeFirst()
            pendingCentralWrites[address] = pending.isEmpty ? nil : pending
            peripheral.writeValue(fragment, for: characteristic, type: .withoutResponse)
        }
    }

    private func flushPeripheralUpdates(address: String) {
        guard let central = subscribedCentrals[address],
              let characteristic = outboundChar else {
            pendingPeripheralUpdates.removeValue(forKey: address)
            return
        }
        while var pending = pendingPeripheralUpdates[address], !pending.isEmpty {
            let fragment = pending[0]
            guard peripheralManager.updateValue(
                fragment,
                for: characteristic,
                onSubscribedCentrals: [central]
            ) else { return }
            pending.removeFirst()
            pendingPeripheralUpdates[address] = pending.isEmpty ? nil : pending
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
        // FI3: if `peripheralManager(_:willRestoreState:)` already re-adopted
        // the mesh service's characteristics (both set below), the service
        // is still registered with the peripheral manager from before the
        // relaunch -- removing and re-adding it here would needlessly drop
        // the `subscribedCentrals` state that was just restored onto
        // `outboundChar`. Only build + register a fresh service when we
        // don't already have one.
        if inboundChar == nil || outboundChar == nil {
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
        }
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

    private func markCentralReadyIfPossible(peripheral: CBPeripheral) {
        let address = peripheral.identifier.uuidString
        guard writeChars[address] != nil,
              notifyChars[address]?.isNotifying == true,
              readyCentralAddresses.insert(address).inserted else { return }
        centralMtuPayload[address] = peripheral.maximumWriteValueLength(for: .withoutResponse)
        onCentralConnected?(address)
    }

    private func scheduleReconnect(peripheral: CBPeripheral, address: String, nowMs: Int64) {
        reconnectWorkItems[address]?.cancel()
        guard running, let delayMs = backoff.retryDelayMs(address: address, nowMs: nowMs) else { return }
        let workItem = DispatchWorkItem { [weak self] in
            guard let self else { return }
            self.reconnectWorkItems.removeValue(forKey: address)
            guard self.running,
                  self.peripheralsById[address] == nil,
                  peripheral.state == .disconnected else { return }
            self.peripheralsById[address] = peripheral
            peripheral.delegate = self
            self.central.connect(peripheral, options: nil)
        }
        reconnectWorkItems[address] = workItem
        queue.asyncAfter(deadline: .now() + .milliseconds(Int(delayMs)), execute: workItem)
    }
}

extension BleTransport: CBCentralManagerDelegate {
    func centralManagerDidUpdateState(_ central: CBCentralManager) {
        if central.state == .poweredOn { startScanIfReady() }
    }

    /// FI3: delivered on a background relaunch after jetsam/termination, for
    /// a `CBCentralManager` created (in `init`) with a restore identifier
    /// that matches a prior session's. `running` starts `false` on a fresh
    /// process, so without this, `startScanIfReady`'s `guard running` would
    /// silently no-op forever until something else calls `start()` -- there
    /// is no guarantee any UI ever appears to do that on a background-only
    /// relaunch. Restoration firing at all is itself proof the mesh was
    /// active when the process died, so it's safe to resume unconditionally
    /// here.
    ///
    /// Every other piece of connection state this transport tracks
    /// (`writeChars`, `notifyChars`, `readyCentralAddresses`,
    /// `centralMtuPayload`, ...) is in-memory only and does not survive the
    /// relaunch, so it is deliberately NOT assumed valid for the restored
    /// peripherals here -- rediscovering services on each one drives the
    /// exact same `didDiscoverServices` -> `didDiscoverCharacteristicsFor`
    /// -> `markCentralReadyIfPossible` chain a fresh connection would, which
    /// re-populates all of it and fires `onCentralConnected` normally.
    func centralManager(_ central: CBCentralManager, willRestoreState dict: [String: Any]) {
        running = true
        let peripherals = dict[CBCentralManagerRestoredStatePeripheralsKey] as? [CBPeripheral] ?? []
        log.info("Central manager restoring state: \(peripherals.count) peripheral(s)")
        for peripheral in peripherals {
            let address = peripheral.identifier.uuidString
            peripheral.delegate = self
            peripheralsById[address] = peripheral
            reassemblers[address] = FrameReassembler()
            if peripheral.state == .connected {
                peripheral.discoverServices([MeshConstants.serviceUUID])
            }
            // Not connected (`.connecting`/`.disconnecting`/`.disconnected`):
            // nothing to rediscover yet -- `didFailToConnect`/
            // `didDisconnectPeripheral` were never delivered for this
            // process, so `scheduleReconnect`'s backoff bookkeeping also
            // never ran; the scan restarted below is what picks it back up.
        }
        // Central.state may still be `.unknown` at this point (Apple
        // delivers `willRestoreState` before the first
        // `centralManagerDidUpdateState`) -- harmless either way, since
        // `startScanIfReady` re-checks `.poweredOn` itself and
        // `centralManagerDidUpdateState` will call it again once state
        // actually changes.
        startScanIfReady()
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
        reconnectWorkItems.removeValue(forKey: address)?.cancel()
        peripheralsById[address] = peripheral
        peripheral.delegate = self
        central.connect(peripheral, options: nil)
    }

    func centralManager(_ central: CBCentralManager, didConnect peripheral: CBPeripheral) {
        let address = peripheral.identifier.uuidString
        reconnectWorkItems.removeValue(forKey: address)?.cancel()
        backoff.recordSuccess(address: address)
        reassemblers[address] = FrameReassembler()
        peripheral.discoverServices([MeshConstants.serviceUUID])
    }

    func centralManager(_ central: CBCentralManager, didFailToConnect peripheral: CBPeripheral, error: Error?) {
        let address = peripheral.identifier.uuidString
        let now = Int64(Date().timeIntervalSince1970 * 1000)
        peripheralsById.removeValue(forKey: address)
        guard running else { return }
        backoff.recordFailure(address: address, nowMs: now)
        scheduleReconnect(peripheral: peripheral, address: address, nowMs: now)
    }

    func centralManager(_ central: CBCentralManager, didDisconnectPeripheral peripheral: CBPeripheral, error: Error?) {
        let address = peripheral.identifier.uuidString
        let now = Int64(Date().timeIntervalSince1970 * 1000)
        peripheralsById.removeValue(forKey: address)
        writeChars.removeValue(forKey: address)
        notifyChars.removeValue(forKey: address)
        reassemblers.removeValue(forKey: address)
        centralMtuPayload.removeValue(forKey: address)
        pendingCentralWrites.removeValue(forKey: address)
        readyCentralAddresses.remove(address)
        guard running else { return }
        backoff.recordFailure(address: address, nowMs: now)
        onCentralDisconnected?(address)
        scheduleReconnect(peripheral: peripheral, address: address, nowMs: now)
    }
}

extension BleTransport: CBPeripheralDelegate {
    func peripheral(_ peripheral: CBPeripheral, didDiscoverServices error: Error?) {
        guard error == nil, let services = peripheral.services else {
            central.cancelPeripheralConnection(peripheral)
            return
        }
        var foundMeshService = false
        for service in services where service.uuid == MeshConstants.serviceUUID {
            foundMeshService = true
            peripheral.discoverCharacteristics(
                [MeshConstants.inboundCharacteristicUUID, MeshConstants.outboundCharacteristicUUID],
                for: service
            )
        }
        if !foundMeshService {
            central.cancelPeripheralConnection(peripheral)
        }
    }

    func peripheral(_ peripheral: CBPeripheral, didDiscoverCharacteristicsFor service: CBService, error: Error?) {
        let address = peripheral.identifier.uuidString
        guard error == nil, let characteristics = service.characteristics else {
            central.cancelPeripheralConnection(peripheral)
            return
        }
        for c in characteristics {
            if c.uuid == MeshConstants.inboundCharacteristicUUID {
                writeChars[address] = c
            } else if c.uuid == MeshConstants.outboundCharacteristicUUID {
                notifyChars[address] = c
                peripheral.setNotifyValue(true, for: c)
            }
        }
        guard writeChars[address] != nil, notifyChars[address] != nil else {
            central.cancelPeripheralConnection(peripheral)
            return
        }
        markCentralReadyIfPossible(peripheral: peripheral)
    }

    func peripheral(_ peripheral: CBPeripheral, didUpdateValueFor characteristic: CBCharacteristic, error: Error?) {
        guard let value = characteristic.value else { return }
        handleFragment(address: peripheral.identifier.uuidString, fragment: value)
    }

    func peripheralIsReady(toSendWriteWithoutResponse peripheral: CBPeripheral) {
        flushCentralWrites(address: peripheral.identifier.uuidString)
    }

    func peripheral(
        _ peripheral: CBPeripheral,
        didUpdateNotificationStateFor characteristic: CBCharacteristic,
        error: Error?
    ) {
        guard characteristic.uuid == MeshConstants.outboundCharacteristicUUID,
              error == nil,
              characteristic.isNotifying else {
            central.cancelPeripheralConnection(peripheral)
            return
        }
        markCentralReadyIfPossible(peripheral: peripheral)
    }
}

extension BleTransport: CBPeripheralManagerDelegate {
    func peripheralManagerDidUpdateState(_ peripheral: CBPeripheralManager) {
        if peripheral.state == .poweredOn { startAdvertisingIfReady() }
    }

    /// FI3: twin of `centralManager(_:willRestoreState:)` for the peripheral
    /// (advertiser) role. Re-adopts the mesh service's characteristics if
    /// the system handed them back so `startAdvertisingIfReady` below can
    /// skip rebuilding the service (see its doc), and re-seeds
    /// `subscribedCentrals`/`peripheralMtuPayload` from
    /// `CBMutableCharacteristic.subscribedCentrals` -- which the system DOES
    /// restore onto the returned characteristic objects, unlike this
    /// transport's own dictionaries -- so `sendAsPeripheral` can resume
    /// writing to already-subscribed centrals without waiting for them to
    /// resubscribe on their own. Fires `onPeripheralSubscribed` for each one,
    /// same as a live `didSubscribeTo` -- `MeshController` needs that to
    /// re-register the route with `MeshRouter` and re-send `Hello`.
    func peripheralManager(_ peripheral: CBPeripheralManager, willRestoreState dict: [String: Any]) {
        running = true
        let services = dict[CBPeripheralManagerRestoredStateServicesKey] as? [CBMutableService] ?? []
        log.info("Peripheral manager restoring state: \(services.count) service(s)")
        for service in services where service.uuid == MeshConstants.serviceUUID {
            for characteristic in service.characteristics ?? [] {
                guard let mutable = characteristic as? CBMutableCharacteristic else { continue }
                if mutable.uuid == MeshConstants.inboundCharacteristicUUID {
                    inboundChar = mutable
                } else if mutable.uuid == MeshConstants.outboundCharacteristicUUID {
                    outboundChar = mutable
                    for central in mutable.subscribedCentrals ?? [] {
                        let address = central.identifier.uuidString
                        subscribedCentrals[address] = central
                        reassemblers[address] = FrameReassembler()
                        peripheralMtuPayload[address] = max(20, central.maximumUpdateValueLength)
                        onPeripheralSubscribed?(address)
                    }
                }
            }
        }
        // Peripheral.state may still be `.unknown` here for the same reason
        // noted in the central twin -- `startAdvertisingIfReady` re-checks
        // `.poweredOn` itself and `peripheralManagerDidUpdateState` will
        // call it again once state actually changes.
        startAdvertisingIfReady()
    }

    func peripheralManager(
        _ peripheral: CBPeripheralManager,
        central: CBCentral,
        didSubscribeTo characteristic: CBCharacteristic
    ) {
        let address = central.identifier.uuidString
        subscribedCentrals[address] = central
        reassemblers[address] = FrameReassembler()
        peripheralMtuPayload[address] = max(20, central.maximumUpdateValueLength)
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
        peripheralMtuPayload.removeValue(forKey: address)
        pendingPeripheralUpdates.removeValue(forKey: address)
        onPeripheralUnsubscribed?(address)
    }

    func peripheralManager(
        _ peripheral: CBPeripheralManager,
        didReceiveWrite requests: [CBATTRequest]
    ) {
        // CoreBluetooth requires exactly one respond(to:) per didReceiveWrite
        // batch, made to the first request (FI10) — process every value,
        // then send the single response for the whole batch.
        guard let first = requests.first else { return }
        var allValid = true
        for request in requests {
            let address = request.central.identifier.uuidString
            if let value = request.value {
                handleFragment(address: address, fragment: value)
            } else {
                allValid = false
            }
        }
        peripheral.respond(to: first, withResult: allValid ? .success : .invalidAttributeValueLength)
    }

    func peripheralManagerIsReady(toUpdateSubscribers peripheral: CBPeripheralManager) {
        for address in Array(pendingPeripheralUpdates.keys) {
            flushPeripheralUpdates(address: address)
        }
    }
}
