import MoqFFI

/// A MoQ client. Configure the optional knobs, then `connect(to:)`.
public final class Client: Sendable {
    let ffi: MoqClient

    public init() {
        ffi = MoqClient()
    }

    /// Toggle TLS certificate verification. Defaults to on; pass `false` only
    /// against a relay with a self-signed certificate during development.
    public func setTlsVerify(_ verify: Bool) {
        ffi.setTlsDisableVerify(disable: !verify)
    }

    /// Set the local UDP socket bind address (defaults to `[::]:0`). Throws if
    /// the address cannot be parsed.
    public func bind(_ addr: String) throws {
        try ffi.setBind(addr: addr)
    }

    /// Wire the origin whose local broadcasts get advertised to the remote. If
    /// left unset, `connect` auto-creates one, reachable via `Session.publisher`.
    public func setPublish(_ origin: OriginProducer?) {
        ffi.setPublish(origin: origin?.ffi)
    }

    /// Wire the origin used to receive the remote's announcements. If left
    /// unset, `connect` auto-creates one, reachable via `Session.consumer`.
    public func setConsume(_ origin: OriginProducer?) {
        ffi.setConsume(origin: origin?.ffi)
    }

    /// Connect and wait for the session to be established. Cancellable via `cancel()`.
    public func connect(to url: String) async throws -> Session {
        Session(try await ffi.connect(url: url))
    }

    /// Cancel all current and future `connect()` calls.
    public func cancel() {
        ffi.cancel()
    }
}

/// An established MoQ session.
public final class Session: Sendable {
    let ffi: MoqSession

    init(_ ffi: MoqSession) {
        self.ffi = ffi
    }

    /// The publish-side origin: where local broadcasts are advertised to the
    /// remote. Either the one wired via `Client.setPublish`, or auto-created.
    public var publisher: OriginProducer {
        OriginProducer(ffi.publisher())
    }

    /// The subscribe-side origin: a read handle for the remote's announcements.
    /// Either derived from `Client.setConsume`, or auto-created.
    public var consumer: OriginConsumer {
        OriginConsumer(ffi.consumer())
    }

    /// Suspend until the session is closed.
    public func closed() async throws {
        try await ffi.closed()
    }

    /// Close the session with the given error code. Code 0 means "no error";
    /// prefer `shutdown()` for that case.
    public func cancel(code: UInt32) {
        ffi.cancel(code: code)
    }

    /// Graceful shutdown. Alias for `cancel(code: 0)`.
    public func shutdown() {
        ffi.shutdown()
    }
}
