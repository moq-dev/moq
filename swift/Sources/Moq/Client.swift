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
    ///   - timeoutUs: How long one connect may take, dial and handshake together; 0 waits forever.
    ///   - raceUs: Happy Eyeballs: how long before the next resolved address is also dialed.
    ///   - resolutionDelayUs: Happy Eyeballs: how long IPv4 waits for the full DNS answer.
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
        timeoutUs: UInt64? = nil,
        raceUs: UInt64? = nil,
        resolutionDelayUs: UInt64? = nil,
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
            timeoutUs: timeoutUs,
            raceUs: raceUs,
            resolutionDelayUs: resolutionDelayUs,
            tls: tls,
            quic: quic,
            websocket: websocket,
            once: once,
            backoff: backoff,
            publish: publish?.ffi,
            consume: consume?.ffi
        ))
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

/// The protocol versions offered by default, most preferred first, spelled the way
/// `Client(versions:)` takes them.
public func versions() -> [String] {
    moqVersions()
}

/// Whether this build can write qlog traces for `QuicConfig.qlog`.
public func qlogSupported() -> Bool {
    moqQlogSupported()
}

/// An established MoQ session.
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

    /// How many times this session has connected: 1 for the connect that built it, one more
    /// on each reconnect. A server-accepted session stays at 1.
    ///
    /// Pair it with `status()` to log each reconnect by number: a `.connected`
    /// status whose count grew is a reconnect.
    public func connects() -> UInt64 {
        ffi.connects()
    }

    /// Why a client session is reconnecting: the error that ended the last
    /// connection or failed the latest redial, or `nil` once a connection lands.
    ///
    /// A redial that keeps failing never changes `status()`, so poll this while it
    /// reports `.disconnected` to show the reason. Giving up for good throws from
    /// `closed()` instead. A server-accepted session always returns `nil`.
    public func failure() -> MoqError? {
        ffi.failure()
    }

    /// Close the session with the given error code. Code 0 means "no error";
    /// prefer `shutdown()` for that case.
    public func cancel(code: UInt32) {
        ffi.cancel(code: code)
    }

    /// Drain finished tracks within one second, throwing on failure.
    /// Explicit `moq-lite-07-wip` waits for the peer to receive finished tails; older
    /// Lite versions, including the default lite-06, wait only for transport ACKs.
    /// IETF also waits for transport ACKs.
    public func shutdown() async throws {
        try await ffi.shutdown()
    }

    /// Snapshot the current connection statistics (RTT, bandwidth estimates,
    /// byte/packet counters). Cheap to call; intended for periodic polling.
    /// Individual fields are `nil` when the transport backend doesn't report them.
    public func stats() -> ConnectionStats {
        ffi.stats()
    }

    /// Hold receive-estimate demand until the returned consumer is released.
    public func recvBandwidth() -> Bandwidth.Consumer? {
        ffi.recvBandwidth().map(Bandwidth.Consumer.init)
    }

    /// The session's bandwidth allocator.
    ///
    /// Every call returns a handle to the same registry, so reservations made
    /// through one are visible to the others. A client handle survives
    /// reconnects: the grant is `nil` while disconnected and resumes on the
    /// next connection.
    public func bandwidth() -> Bandwidth {
        Bandwidth(ffi.bandwidth())
    }
}
