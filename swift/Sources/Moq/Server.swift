import MoqFFI

/// A MoQ server that accepts incoming QUIC/WebTransport sessions.
public final class Server: Sendable {
    let ffi: MoqServer

    /// Create a server. Nothing binds until `listen()`.
    ///
    /// - Parameters:
    ///   - bind: The address to bind, e.g. `127.0.0.1:4443` or `localhost:0`; `nil` binds
    ///     `[::]:443`. DNS hostnames resolve at `listen()`.
    ///   - versions: Protocol versions to accept (`"moq-lite-03"`); empty accepts every
    ///     supported version.
    ///   - tls: The served identity: PEM `cert`/`key` files, or hostnames to `generate`
    ///     a self-signed certificate for (clients then pin `certFingerprints` or skip verification).
    ///   - quic: QUIC tuning, such as each peer's inbound stream cap.
    ///   - publish: The origin whose broadcasts are served to incoming sessions.
    ///   - consume: The origin that receives broadcasts published by incoming sessions.
    /// - Throws: `MoqError.Config` for a value the native side cannot use.
    public init(
        bind: String? = nil,
        versions: [String] = [],
        tls: ServerTls = ServerTls(),
        quic: QuicConfig = QuicConfig(),
        publish: OriginProducer? = nil,
        consume: OriginProducer? = nil
    ) throws {
        ffi = try MoqServer(config: MoqServerConfig(
            bind: bind,
            versions: versions,
            tls: tls,
            quic: quic,
            publish: publish?.ffi,
            consume: consume?.ffi
        ))
    }

    /// Bind the listening socket. Returns the bound local address, useful when
    /// binding to an ephemeral port (`:0`).
    public func listen() async throws -> String {
        try await ffi.listen()
    }

    /// Accept the next incoming session. Returns `nil` once the server closes.
    /// `listen()` must be called first.
    public func accept() async throws -> Request? {
        (try await ffi.accept()).map(Request.init)
    }

    /// SHA-256 fingerprints of the configured TLS certificates, hex-encoded.
    /// Useful for pinning a generated self-signed cert in a WebTransport client.
    public func certFingerprints() throws -> [String] {
        try ffi.certFingerprints()
    }

    /// Cancel any in-flight `listen()` or `accept()` call.
    ///
    /// Returns once the listening socket is closed, so the address can be bound
    /// again immediately.
    public func cancel() {
        ffi.cancel()
    }
}

/// An incoming MoQ session that can be accepted or rejected.
public final class Request: Sendable {
    let ffi: MoqRequest

    init(_ ffi: MoqRequest) {
        self.ffi = ffi
    }

    /// The URL provided by the client, if any.
    public var url: String? {
        ffi.url()
    }

    /// The query-free request path, or an empty string for the root/missing path.
    public var path: String {
        ffi.path()
    }

    /// The encoded request query without `?`; it may contain credentials.
    public var query: String? {
        ffi.query()
    }

    /// The network transport carrying this session.
    public var transport: Transport {
        ffi.transport()
    }

    /// Complete the handshake, inheriting server origins wherever an argument is nil.
    public func accept(publish: OriginProducer? = nil, consume: OriginProducer? = nil) async throws -> Session {
        Session(try await ffi.accept(publish: publish?.ffi, consume: consume?.ffi))
    }

    /// Reject the session with an application error code; 401 and 403 map to unauthorized.
    public func reject(code: UInt16) async throws {
        try await ffi.reject(code: code)
    }

    /// Cancel any in-flight `accept()` or `reject()` call.
    public func cancel() {
        ffi.cancel()
    }
}
