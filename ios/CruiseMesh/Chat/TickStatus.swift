import Foundation

enum TickStatus {
    case sent
    case delivered
    case read
}

func tickStatusFor(lamport: UInt64, deliveredThrough: UInt64, readThrough: UInt64) -> TickStatus {
    switch coreTickStatusFor(lamport: lamport, deliveredThrough: deliveredThrough, readThrough: readThrough) {
    case .sent: return .sent
    case .delivered: return .delivered
    case .read: return .read
    }
}

func tickLegendText(_ status: TickStatus) -> String {
    switch status {
    case .sent:
        return "Sent — sealed and queued for the mesh. Waiting for the recipient's device."
    case .delivered:
        return "Delivered — their phone decrypted and stored this message."
    case .read:
        return "Read — they opened this chat."
    }
}
