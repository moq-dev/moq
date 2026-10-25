import Foundation
import MoqFFI

/// A MoQ client, built from its configuration and then dialed with `connect(to:)`.
public final class Client: Sendable {
    let ffi: MoqClient

    /// Create a client. Every argument has a default, so pass only what you need.
    ///
    /// - Parameters:
    ///   - bind: The local UDP address to bind; `nil` binds an ephemeral dual-stack port.
    ///   - versions: Protocol versions to offer, most preferred first (`"moq-lite-03"`);
    ///     empty offers every supported version, which `versions()` lists.
    ///   - timeout: How long one connect may take, dial and handshake together; 0 waits forever.
    ///   - race: Happy Eyeballs: how long before the next resolved address is also dialed.
    ///   - resolutionDelay: Happy Eyeballs: how long IPv4 waits for the full DNS answer.
    ///   - tls: Certificate trust (`insecure`, `roots`, `fingerprints`, ...), the SNI override,
    ///     and the mTLS identity.
    ///   - quic: QUIC tuning, such as the peer's inbound stream cap and the idle timeout.
    ///   - websocket: The WebSocket fallback raced against QUIC for `http(s)` URLs.
    ///   - once: Dial once instead of redialing with backoff whenever the transport drops.
    ///   - backoff: Retry pacing for the automatic reconnect.
    ///   - publish: The origin whose broadcasts are published to the remote.
    ///   - consume: The origin that receives the remote's broadcasts. With neither origin
    ///     given, both sides of each session share one, so a broadcast announced via
    ///     `Session.publish` is also discoverable through `Session.consume`.
    /// - Throws: `MoqError.Config` for a value the native side cannot use.
    public init(
        bind: String? = nil,
        versions: [String] = [],
        timeout: TimeInterval? = nil,
        race: TimeInterval? = nil,
        resolutionDelay: TimeInterval? = nil,
        tls: ClientTls = ClientTls(),
        quic: QuicConfig = QuicConfig(),
        websocket: WebSocketConfig = WebSocketConfig(),
        once: Bool = false,
        backoff: Backoff = Backoff(),
        publish: OriginProducer? = nil,
        consume: OriginProducer? = nil
    ) throws {
        ffi = try MoqClient(config: MoqClientConfig(
            bind: bind,
            versions: versions,
            timeout: timeout,
            race: race,
            resolutionDelay: resolutionDelay,
            tls: tls,
            quic: quic,
            websocket: websocket,
            once: once,
            backoff: backoff,
            implementation: implementation,
            publish: publish?.ffi,
            consume: consume?.ffi
        ))
    }

    /// Start a connection and return its session without waiting for the native first dial to succeed.
    public func connect(to url: String) async throws -> Session {
        Session(try await ffi.connect(url: url))
    }

    /// Cancel all current and future `connect()` calls.
    public func cancel() {
        ffi.cancel()
    }


}

/// The protocol versions offered by default, most preferred first, spelled the way
/// `Client(versions:)` takes them.
public func versions() -> [String] {
    moqVersions()
}

/// Whether this build can write qlog traces for `QuicConfig.qlog`.
public func qlogSupported() -> Bool {
    moqQlogSupported()
}

/// A MoQ session, including its initial native dial.
public final class Session: Sendable {
    let ffi: MoqSession

    init(_ ffi: MoqSession) {
        self.ffi = ffi
    }

    /// The publish-side origin: where local broadcasts are advertised to the
    /// remote. Either the one wired via `Client(publish:)`, or auto-created.
    public var publish: OriginProducer {
        OriginProducer(ffi.publish())
    }

    /// The subscribe-side origin: a read handle for the remote's announcements.
    /// Either derived from `Client(consume:)`, or auto-created.
    public var consume: OriginConsumer {
        OriginConsumer(ffi.consume())
    }

    /// Suspend until the session is over: an error when the connection gave up for
    /// good, a normal return after a local `cancel()`. After `shutdown()`, wait for
    /// the drain and throw on delivery failure. Transient drops
    /// the reconnect loop rides out don't resolve this; watch `stateChanged(after:)` for those.
    public func closed() async throws {
        try await ffi.closed()
    }

    /// Read one coherent lifecycle snapshot without advancing a watcher.
    public func state() -> ConnectionState {
        ffi.state()
    }

    /// Wait for a revision different from `after`; independent callers retain their own revisions.
    public func stateChanged(after: UInt64) async throws -> ConnectionState {
        try await ffi.stateChanged(after: after)
    }

    /// Close the session with the given error code. Code 0 means "no error";
    /// prefer `shutdown()` for that case.
    public func cancel(code: UInt32) {
        ffi.cancel(code: code)
    }

    /// Request a draining close now; await `closed()` for its result.
    /// Explicit `moq-lite-07-wip` waits for the peer to receive finished tails; older
    /// Lite versions, including the default lite-06, wait only for transport ACKs.
    /// IETF also waits for transport ACKs.
    public func shutdown() {
        ffi.shutdown()
    }

    /// Snapshot the current connection statistics (RTT, bandwidth estimates,
    /// byte/packet counters). Cheap to call; intended for periodic polling.
    /// Individual fields are `nil` when the transport backend doesn't report them.
    public func stats() -> ConnectionStats {
        ffi.stats()
    }

    /// The session's authorization: the grant its peer gave it, and more tokens to present.
    ///
    /// Each call returns an independent watcher of the grant. A client session's
    /// handle survives reconnects, presenting each added token again on every new
    /// connection.
    public func auth() -> Auth {
        Auth(ffi.auth())
    }

    /// Hold receive-estimate demand until the returned consumer is released.
    public func recvBandwidth() -> BandwidthConsumer? {
        ffi.recvBandwidth().map(BandwidthConsumer.init)
    }

    /// The session's bandwidth allocator.
    ///
    /// Every call returns a handle to the same registry, so reservations made
    /// through one are visible to the others. A client handle survives
    /// reconnects: the grant is `nil` while disconnected and resumes on the
    /// next connection.
    public func allocator() -> Allocator {
        Allocator(ffi.allocator())
    }
}
