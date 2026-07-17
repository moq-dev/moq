import MoqFFI

/// A MoQ server that accepts incoming QUIC/WebTransport sessions.
public final class Server: Sendable {
    let ffi: MoqServer

    public init() {
        ffi = MoqServer()
    }

    /// Set the address to bind, e.g. `127.0.0.1:4443`, `[::]:443`, or `localhost:0`.
    /// Validated syntactically here; DNS hostnames resolve at `listen()` time.
    public func bind(_ addr: String) throws {
        try ffi.setBind(addr: addr)
    }

    /// Load TLS certificate chains from PEM files on disk.
    public func setTlsCert(_ paths: [String]) {
        ffi.setTlsCert(paths: paths)
    }

    /// Load TLS private keys from PEM files on disk.
    public func setTlsKey(_ paths: [String]) {
        ffi.setTlsKey(paths: paths)
    }

    /// Generate self-signed TLS certificates for the given hostnames. Clients
    /// must pin the fingerprint (see `certFingerprints`) or disable verification.
    public func generateTls(hostnames: [String]) {
        ffi.setTlsGenerate(hostnames: hostnames)
    }

    /// Set the origin to publish broadcasts to incoming sessions.
    public func setPublish(_ origin: OriginProducer?) {
        ffi.setPublish(origin: origin?.ffi)
    }

    /// Set the origin to consume broadcasts from incoming sessions.
    public func setConsume(_ origin: OriginProducer?) {
        ffi.setConsume(origin: origin?.ffi)
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

    /// The transport type, e.g. `"quic"`, `"iroh"`, or `"websocket"`.
    public var transport: String {
        ffi.transport()
    }

    /// Override the publish origin for this session, falling back to the server's.
    public func setPublish(_ origin: OriginProducer?) {
        ffi.setPublish(origin: origin?.ffi)
    }

    /// Override the consume origin for this session, falling back to the server's.
    public func setConsume(_ origin: OriginProducer?) {
        ffi.setConsume(origin: origin?.ffi)
    }

    /// Complete the handshake and return the established session.
    public func accept() async throws -> Session {
        Session(try await ffi.accept())
    }

    /// Reject the session with the given HTTP status code.
    public func reject(code: UInt16) async throws {
        try await ffi.reject(code: code)
    }

    /// Cancel any in-flight `accept()` or `reject()` call.
    public func cancel() {
        ffi.cancel()
    }
}
