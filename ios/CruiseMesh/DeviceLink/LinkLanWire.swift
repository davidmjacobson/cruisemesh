import Darwin
import Foundation

/// The §13 gate's LAN leg: a plain TCP socket between the two phones, carrying
/// length-framed ceremony messages.
///
/// There is deliberately no Noise here, and no contact check. The mesh's same-LAN
/// transport (`LanTransport`) promotes a socket only once the peer's Noise static
/// key matches an accepted contact — but the two devices in a link ceremony are
/// not contacts and never will be, and the key they must match is the ephemeral
/// one printed in the QR. That check belongs to the ceremony, which makes it
/// (`CoreLinkApprovingDevice` refuses any peer whose static is not the scanned
/// key) on a channel it establishes itself. This socket's whole job is to move
/// opaque bytes.
///
/// Which means: this socket is untrusted, and is treated that way. Every read is
/// bounded, every message is length-capped, and nothing that arrives on it is
/// interpreted anywhere but inside the ceremony.
///
/// # Why BSD sockets and not `Network.framework`
///
/// The ceremony driver is synchronous by design — core hands out one outstanding
/// action at a time and is resumed with exactly what arrived. A blocking socket
/// with `SO_RCVTIMEO` expresses `LinkWire.receive(waitMs:)` directly; wrapping
/// `NWConnection`'s callbacks back into the same bounded blocking shape would add
/// a semaphore and a state machine around an API that already has one. Everything
/// here runs on `LinkSession`'s own background thread and never on the main queue.
///
/// Mirrors Android's `LinkLanWire.kt`.
final class LinkLanWire: LinkWire {
    private let fd: Int32
    private let sendLock = NSLock()
    private var closed = false

    /// How long a body read may take once its length header has arrived. Long
    /// enough not to tear a message in half, still bounded so a peer that stops
    /// mid-message cannot hold the thread.
    private static let bodyReadTimeoutMs: Int64 = 15_000

    init(fd: Int32) {
        self.fd = fd
        // A peer that vanishes mid-write must not take the app down with
        // SIGPIPE. Darwin has no MSG_NOSIGNAL, so this is the socket option.
        var on: Int32 = 1
        setsockopt(fd, SOL_SOCKET, SO_NOSIGPIPE, &on, socklen_t(MemoryLayout<Int32>.size))
    }

    deinit { close() }

    func send(_ bytes: Data) throws {
        guard bytes.count <= LinkWireLimits.maxMessageBytes else {
            throw LinkWireError.tooLarge(bytes.count)
        }
        sendLock.lock()
        defer { sendLock.unlock() }
        // Four-byte big-endian length, spelled out a byte at a time rather than
        // reinterpreted through a pointer: the same header Android's
        // `DataOutputStream.writeInt` writes, and no endianness of this device's
        // can change what goes on the wire.
        let length = UInt32(bytes.count)
        var frame = Data([
            UInt8(truncatingIfNeeded: length >> 24),
            UInt8(truncatingIfNeeded: length >> 16),
            UInt8(truncatingIfNeeded: length >> 8),
            UInt8(truncatingIfNeeded: length),
        ])
        frame.append(bytes)
        try writeFully(frame)
    }

    func receive(waitMs: Int64) throws -> Data? {
        setReceiveTimeout(ms: min(max(waitMs, 1), LinkWireLimits.maxReceiveWaitMs))
        guard let header = try readFully(4, allowTimeout: true) else { return nil }
        let length = (Int(header[0]) << 24) | (Int(header[1]) << 16)
            | (Int(header[2]) << 8) | Int(header[3])
        guard length > 0, length <= LinkWireLimits.maxMessageBytes else {
            throw LinkWireError.badLength(length)
        }
        // The header arrived, so the body is on its way: read it without a
        // per-read deadline short enough to tear a message in half.
        setReceiveTimeout(ms: Self.bodyReadTimeoutMs)
        guard let body = try readFully(length, allowTimeout: false) else {
            throw LinkWireError.peerClosed
        }
        return body
    }

    func close() {
        sendLock.lock()
        defer { sendLock.unlock() }
        guard !closed else { return }
        closed = true
        _ = Darwin.close(fd)
    }

    private func setReceiveTimeout(ms: Int64) {
        var timeout = timeval(
            tv_sec: Int(ms / 1_000),
            tv_usec: Int32((ms % 1_000) * 1_000)
        )
        setsockopt(fd, SOL_SOCKET, SO_RCVTIMEO, &timeout, socklen_t(MemoryLayout<timeval>.size))
    }

    /// Reads exactly `count` bytes. Returns nil only when the *first* read timed
    /// out and `allowTimeout` is set — nothing of this message has arrived yet,
    /// which is the driver's cue to tick rather than to fail.
    private func readFully(_ count: Int, allowTimeout: Bool) throws -> Data? {
        var buffer = [UInt8](repeating: 0, count: count)
        var filled = 0
        while filled < count {
            let read: Int = buffer.withUnsafeMutableBytes { raw -> Int in
                guard let base = raw.baseAddress else { return -1 }
                return Darwin.recv(fd, base.advanced(by: filled), count - filled, 0)
            }
            if read > 0 {
                filled += read
                continue
            }
            if read == 0 {
                // A clean close is never "nothing yet": Android's `readInt`
                // raises EOF here too, and a wire that answered nil would leave
                // the driver ticking against a dead socket until the ceremony's
                // own deadline instead of failing at the moment it broke.
                throw LinkWireError.peerClosed
            }
            let code = errno
            if code == EINTR { continue }
            if code == EAGAIN || code == EWOULDBLOCK {
                if filled == 0, allowTimeout { return nil }
                throw LinkWireError.transport("the other device stopped mid-message")
            }
            throw LinkWireError.transport("link socket read failed (errno \(code))")
        }
        return Data(buffer)
    }

    private func writeFully(_ bytes: Data) throws {
        var written = 0
        try bytes.withUnsafeBytes { raw in
            guard let base = raw.baseAddress else { throw LinkWireError.notConnected }
            while written < bytes.count {
                let sent = Darwin.send(fd, base.advanced(by: written), bytes.count - written, 0)
                if sent > 0 {
                    written += sent
                    continue
                }
                let code = errno
                if code == EINTR { continue }
                throw LinkWireError.transport("link socket write failed (errno \(code))")
            }
        }
    }
}

/// The new device's side of the LAN leg: a listener on an ephemeral port whose
/// address goes into the QR (§9.1).
///
/// The endpoints it publishes are this device's own, and only this device's — the
/// QR is an invitation to knock here, never a report of what else is on the
/// network (DL-5's rule, one layer out).
final class LinkLanListener {
    private let fd: Int32
    /// `host:port` as the core formats them, ready for the QR's hints.
    let endpoints: [String]
    private var closed = false

    private init(fd: Int32, port: UInt16) {
        self.fd = fd
        self.endpoints = LinkLanListener.localIPv4Addresses().map { host in
            coreFormatLanEndpoint(endpoint: CoreLanEndpoint(host: host, port: port))
        }
    }

    deinit { close() }

    static func open() throws -> LinkLanListener {
        let fd = socket(AF_INET, SOCK_STREAM, 0)
        guard fd >= 0 else {
            throw LinkWireError.transport("could not open a link socket (errno \(errno))")
        }
        var on: Int32 = 1
        setsockopt(fd, SOL_SOCKET, SO_REUSEADDR, &on, socklen_t(MemoryLayout<Int32>.size))

        var address = sockaddr_in()
        address.sin_len = UInt8(MemoryLayout<sockaddr_in>.size)
        address.sin_family = sa_family_t(AF_INET)
        address.sin_port = 0
        address.sin_addr.s_addr = in_addr_t(0)
        let bound = withUnsafePointer(to: &address) { pointer in
            pointer.withMemoryRebound(to: sockaddr.self, capacity: 1) { raw in
                Darwin.bind(fd, raw, socklen_t(MemoryLayout<sockaddr_in>.size))
            }
        }
        guard bound == 0, Darwin.listen(fd, 1) == 0 else {
            _ = Darwin.close(fd)
            throw LinkWireError.transport("could not listen for the other device (errno \(errno))")
        }

        var boundAddress = sockaddr_in()
        var length = socklen_t(MemoryLayout<sockaddr_in>.size)
        let named = withUnsafeMutablePointer(to: &boundAddress) { pointer in
            pointer.withMemoryRebound(to: sockaddr.self, capacity: 1) { raw in
                getsockname(fd, raw, &length)
            }
        }
        guard named == 0 else {
            _ = Darwin.close(fd)
            throw LinkWireError.transport("could not read the link socket's port (errno \(errno))")
        }
        return LinkLanListener(fd: fd, port: UInt16(bigEndian: boundAddress.sin_port))
    }

    /// Wait for the approving device to connect, or return nil if nobody came
    /// within `waitMs`. Called in a loop by the session so that the ceremony's
    /// own deadline stays the only clock that matters.
    func accept(waitMs: Int64) throws -> LinkLanWire? {
        let bounded = min(max(waitMs, 1), LinkWireLimits.maxReceiveWaitMs)
        var poller = pollfd(fd: fd, events: Int16(POLLIN), revents: 0)
        let ready = poll(&poller, 1, Int32(bounded))
        if ready == 0 { return nil }
        if ready < 0 {
            if errno == EINTR { return nil }
            throw LinkWireError.transport("could not wait for the other device (errno \(errno))")
        }
        let accepted = Darwin.accept(fd, nil, nil)
        guard accepted >= 0 else {
            if errno == EAGAIN || errno == EWOULDBLOCK || errno == EINTR { return nil }
            throw LinkWireError.transport("the other device could not connect (errno \(errno))")
        }
        return LinkLanWire(fd: accepted)
    }

    func close() {
        guard !closed else { return }
        closed = true
        _ = Darwin.close(fd)
    }

    /// This device's own routable IPv4 addresses. IPv4 only, and not because IPv6
    /// is unwelcome: the QR is a few hundred bytes and a link-local IPv6 address
    /// needs a scope id the other phone cannot use anyway.
    static func localIPv4Addresses() -> [String] {
        var hosts: [String] = []
        var first: UnsafeMutablePointer<ifaddrs>?
        guard getifaddrs(&first) == 0, let first else { return hosts }
        defer { freeifaddrs(first) }

        var cursor: UnsafeMutablePointer<ifaddrs>? = first
        while let current = cursor {
            defer { cursor = current.pointee.ifa_next }
            let flags = Int32(current.pointee.ifa_flags)
            guard flags & IFF_UP != 0, flags & IFF_LOOPBACK == 0 else { continue }
            guard let address = current.pointee.ifa_addr,
                  address.pointee.sa_family == UInt8(AF_INET) else { continue }
            var host = [CChar](repeating: 0, count: Int(NI_MAXHOST))
            let named = getnameinfo(
                address,
                socklen_t(address.pointee.sa_len),
                &host,
                socklen_t(host.count),
                nil,
                0,
                NI_NUMERICHOST
            )
            guard named == 0 else { continue }
            let text = String(cString: host)
            // 169.254/16 is link-local: reachable only by accident, and never
            // the address the other phone is standing on.
            guard !text.isEmpty, !text.hasPrefix("169.254."), !hosts.contains(text) else { continue }
            hosts.append(text)
        }
        return hosts
    }
}

/// The listener as a `LinkWire` that accepts on first use.
///
/// This is what lets the offer's own expiry be the only clock in the room. The
/// new device's first action is "show this QR", answered by a bounded wait for
/// the peer; if nobody knocks, the wait returns nil, the driver ticks, and the
/// *core* decides whether the offer has expired. A shell that ran its own accept
/// loop first would be a shell inventing a second timeout beside the one §9.2
/// declares.
final class LinkLanAcceptingWire: LinkWire {
    private let listener: LinkLanListener
    private var accepted: LinkLanWire?

    init(listener: LinkLanListener) { self.listener = listener }

    func send(_ bytes: Data) throws {
        guard let accepted else { throw LinkWireError.notConnected }
        try accepted.send(bytes)
    }

    func receive(waitMs: Int64) throws -> Data? {
        if accepted == nil {
            guard let wire = try listener.accept(waitMs: waitMs) else { return nil }
            accepted = wire
        }
        return try accepted?.receive(waitMs: waitMs)
    }

    func close() {
        accepted?.close()
        listener.close()
    }
}

enum LinkLanDialer {
    /// Dial one of the endpoints the QR advertised, trying them in order: a
    /// device with two interfaces publishes both, and only one of them is on the
    /// network the scanner is standing in.
    static func connect(endpoints: [String], connectTimeoutMs: Int64) throws -> LinkLanWire {
        var failure: Error?
        for text in endpoints {
            guard let endpoint = coreParseLanEndpoint(text: text, defaultPort: 0),
                  endpoint.port != 0 else { continue }
            do {
                return try dial(host: endpoint.host, port: endpoint.port, timeoutMs: connectTimeoutMs)
            } catch {
                failure = error
            }
        }
        throw failure ?? LinkWireError.transport("the offer carries no reachable Wi-Fi address")
    }

    /// A bounded connect: non-blocking `connect`, then `poll` for writability,
    /// then `SO_ERROR`. `SO_SNDTIMEO` is deliberately not used — its effect on a
    /// blocking `connect` is not something to bet a ceremony's only clock on.
    private static func dial(host: String, port: UInt16, timeoutMs: Int64) throws -> LinkLanWire {
        var address = sockaddr_in()
        address.sin_len = UInt8(MemoryLayout<sockaddr_in>.size)
        address.sin_family = sa_family_t(AF_INET)
        address.sin_port = port.bigEndian
        guard inet_pton(AF_INET, host, &address.sin_addr) == 1 else {
            throw LinkWireError.transport("the offer's address could not be read")
        }

        let fd = socket(AF_INET, SOCK_STREAM, 0)
        guard fd >= 0 else {
            throw LinkWireError.transport("could not open a link socket (errno \(errno))")
        }
        var succeeded = false
        defer { if !succeeded { _ = Darwin.close(fd) } }

        let originalFlags = fcntl(fd, F_GETFL, 0)
        _ = fcntl(fd, F_SETFL, originalFlags | O_NONBLOCK)

        let started = withUnsafePointer(to: &address) { pointer in
            pointer.withMemoryRebound(to: sockaddr.self, capacity: 1) { raw in
                Darwin.connect(fd, raw, socklen_t(MemoryLayout<sockaddr_in>.size))
            }
        }
        if started != 0 {
            guard errno == EINPROGRESS else {
                throw LinkWireError.transport("could not reach the other device (errno \(errno))")
            }
            var poller = pollfd(fd: fd, events: Int16(POLLOUT), revents: 0)
            let ready = poll(&poller, 1, Int32(max(timeoutMs, 1)))
            guard ready > 0 else {
                throw LinkWireError.transport("the other device did not answer on Wi-Fi")
            }
            var socketError: Int32 = 0
            var length = socklen_t(MemoryLayout<Int32>.size)
            guard getsockopt(fd, SOL_SOCKET, SO_ERROR, &socketError, &length) == 0,
                  socketError == 0 else {
                throw LinkWireError.transport("could not reach the other device (errno \(socketError))")
            }
        }
        _ = fcntl(fd, F_SETFL, originalFlags)
        succeeded = true
        return LinkLanWire(fd: fd)
    }
}
