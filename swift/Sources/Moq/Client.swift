import MoqFFI

/// A MoQ client. Configure the optional knobs, then `connect(to:)`.
public final class Client: Sendable {
    let ffi: MoqClient

    /// Create a client with default settings, ready to configure and `connect(to:)`.
    public init() {
        ffi = MoqClient()
    }

    /// Toggle TLS certificate verification. Defaults to on; pass `false` only
    /// against a relay with a self-signed certificate during development.
    public func setTlsVerify(_ verify: Bool) {
        ffi.setTlsDisableVerify(disable: !verify)
    }

    /// Trust these PEM root certificate file path(s) instead of the system roots.
    public func setTlsRoots(_ paths: [String]) {
        ffi.setTlsRoots(paths: paths)
    }

    /// Configure whether platform roots are trusted in addition to custom roots.
    public func setTlsSystemRoots(_ enabled: Bool) {
        ffi.setTlsSystemRoots(systemRoots: enabled)
    }

    /// Pin the peer certificate to these hex SHA-256 fingerprints, the native
    /// equivalent of `serverCertificateHashes`. Accepts the values a server
    /// reports via `Server.certFingerprints`, so a self-signed certificate can be
    /// trusted without disabling verification.
    public func setTlsFingerprints(_ fingerprints: [String]) {
        ffi.setTlsFingerprints(fingerprints: fingerprints)
    }

    /// Set the path to a PEM certificate chain to present when the relay requires mTLS.
    public func setTlsCert(_ path: String?) {
        ffi.setTlsCert(path: path)
    }

    /// Set the path to a PEM private key to present when the relay requires mTLS.
    public func setTlsKey(_ path: String?) {
        ffi.setTlsKey(path: path)
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

    /// Enable or disable automatic reconnecting (on by default). When enabled, the
    /// session redials with backoff whenever the transport drops, and broadcasts
    /// consumed through it ride out the gap. Disable for a one-shot dial whose
    /// transport close ends the session.
    public func setReconnect(_ enabled: Bool) {
        ffi.setReconnect(enabled: enabled)
    }

    /// Configure retry pacing for the automatic reconnect.
    public func setBackoff(_ backoff: Backoff) {
        ffi.setBackoff(backoff: backoff)
    }

    /// Connect and wait for the session to be established. Cancellable via `cancel()`.
    ///
    /// With neither `setPublish` nor `setConsume` wired, both sides of the session share one
    /// origin, so a broadcast announced via `Session.publisher` is also discoverable through
    /// `Session.consumer`. Wiring either side opts out and isolates the two directions.
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

    /// Suspend until the session is over: an error when the connection gave up for
    /// good, a normal return after a local `shutdown()`/`cancel()`. Transient drops
    /// the reconnect loop rides out don't resolve this; watch `status()` for those.
    public func closed() async throws {
        try await ffi.closed()
    }

    /// Suspend until the connection status differs from the one last reported. A
    /// client session reports `.connected` first, then follows the reconnect loop
    /// (`.disconnected` while redialing, `.migrating` during a GOAWAY handover) and
    /// throws once the connection stops for good. A server-accepted session's only
    /// transition is terminal: this waits for the close and throws its reason.
    ///
    /// This is the current status, not a queue of every edge: a drop that
    /// reconnects before you ask again is coalesced away, so the outages it hides
    /// are the ones that already healed. Don't count outages with it.
    public func status() async throws -> ConnectionStatus {
        try await ffi.status()
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

    /// Snapshot the current connection statistics (RTT, bandwidth estimates,
    /// byte/packet counters). Cheap to call; intended for periodic polling.
    /// Individual fields are `nil` when the transport backend doesn't report them.
    public func stats() -> ConnectionStats {
        ffi.stats()
    }
}
