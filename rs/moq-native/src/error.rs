use std::path::PathBuf;

/// Errors produced while configuring or establishing native MoQ connections.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
	// ── Upstream (transparent) ──────────────────────────────────────────
	#[error(transparent)]
	Io(#[from] std::io::Error),

	#[error(transparent)]
	MoqNet(#[from] moq_net::Error),

	#[error(transparent)]
	Rustls(#[from] rustls::Error),

	#[error(transparent)]
	Url(#[from] url::ParseError),

	#[error("failed to decode ALPN")]
	DecodeAlpn(#[from] std::string::FromUtf8Error),

	#[cfg(feature = "quiche")]
	#[error("failed to decode ALPN")]
	DecodeAlpnUtf8(#[from] std::str::Utf8Error),

	#[error("invalid fingerprint")]
	InvalidFingerprint(#[from] hex::FromHexError),

	#[error("invalid log directive")]
	Directive(#[from] tracing_subscriber::filter::ParseError),

	#[error("failed to set global tracing subscriber")]
	SetSubscriber(#[source] tracing_subscriber::util::TryInitError),

	#[error("failed to initialize Android logcat layer")]
	Logcat(#[source] std::io::Error),

	// ── Backend selection ───────────────────────────────────────────────
	#[error("{0}")]
	NoBackend(&'static str),

	#[error("failed to connect to server")]
	ConnectFailed,

	#[cfg(feature = "iroh")]
	#[error("Iroh support is not enabled")]
	IrohDisabled,

	// ── TLS / certificates ──────────────────────────────────────────────
	#[error("failed to open certificate file")]
	OpenCert(#[source] std::io::Error),

	#[error("failed to read file")]
	ReadFile(#[source] std::io::Error),

	#[error("failed to read certificates")]
	ReadCerts(#[source] rustls::pki_types::pem::Error),

	#[error("failed to parse private key")]
	KeyPem(#[source] rustls::pki_types::pem::Error),

	#[error("no certificates found")]
	NoCerts,

	#[error("no roots found in {}", .0.display())]
	EmptyRoots(PathBuf),

	#[error("failed to add root certificate")]
	AddRoot(#[source] rustls::Error),

	#[error("failed to configure client certificate")]
	ClientAuth(#[source] rustls::Error),

	#[error("both --client-tls-cert and --client-tls-key must be provided")]
	IncompleteClientAuth,

	#[error("must provide both cert and key")]
	CertKeyCountMismatch,

	#[error("must provide at least one cert/key pair or generate entry")]
	NoCertSource,

	#[error("private key {} doesn't match certificate {}", key.display(), cert.display())]
	KeyMismatch {
		key: PathBuf,
		cert: PathBuf,
		#[source]
		source: rustls::Error,
	},

	#[cfg(any(feature = "quinn", feature = "noq", feature = "quiche"))]
	#[error(transparent)]
	Rcgen(#[from] rcgen::Error),

	#[error("no crypto provider available; enable aws-lc-rs or ring feature")]
	NoCryptoProvider,

	#[error("tls.root (mTLS) is only supported by the quinn backend")]
	MtlsQuinnOnly,

	#[error("invalid status code")]
	InvalidStatusCode,

	// ── DNS / addresses ─────────────────────────────────────────────────
	#[error("invalid address")]
	ResolveAddr(#[source] std::io::Error),

	#[error("no addresses resolved")]
	NoAddresses,

	#[error("failed to resolve bind address")]
	ResolveBind(#[source] Box<Error>),

	#[error("failed to bind UDP socket")]
	BindSocket(#[source] std::io::Error),

	#[error("failed to create QUIC endpoint")]
	CreateEndpoint(#[source] std::io::Error),

	#[error("no async runtime")]
	NoRuntime,

	#[error("failed to get local address")]
	LocalAddr(#[source] std::io::Error),

	#[cfg(feature = "quiche")]
	#[error("failed to get local address")]
	NoLocalAddr,

	#[error("invalid DNS name")]
	InvalidDnsName,

	#[error("failed DNS lookup")]
	DnsLookup(#[source] std::io::Error),

	#[error("no DNS entries")]
	NoDnsEntries,

	// ── URL / scheme / ALPN ─────────────────────────────────────────────
	#[error("url scheme must be 'https', 'moqt', or 'moql'")]
	InvalidScheme,

	#[error("unsupported URL scheme: {0}")]
	UnsupportedScheme(String),

	#[error("missing handshake data")]
	MissingHandshake,

	#[error("missing ALPN")]
	MissingAlpn,

	#[error("unsupported ALPN: {0}")]
	UnsupportedAlpn(String),

	#[error("missing server name for raw QUIC connection")]
	MissingServerName,

	#[error("failed to construct URL from server name")]
	BuildUrl(#[source] url::ParseError),

	// ── QUIC-LB config ──────────────────────────────────────────────────
	#[error("quic_lb_nonce must be at least 4")]
	QuicLbNonceTooSmall,

	#[error("connection ID length ({0}) exceeds maximum of 20")]
	QuicLbCidTooLong(usize),

	// ── Fingerprint (insecure HTTP) ─────────────────────────────────────
	#[cfg(any(feature = "quinn", feature = "noq"))]
	#[error("failed to fetch fingerprint")]
	FetchFingerprint(#[source] reqwest::Error),

	#[cfg(any(feature = "quinn", feature = "noq"))]
	#[error("fingerprint request failed")]
	FingerprintStatus(#[source] reqwest::Error),

	#[cfg(any(feature = "quinn", feature = "noq"))]
	#[error("failed to read fingerprint")]
	ReadFingerprint(#[source] reqwest::Error),

	// ── quinn backend ───────────────────────────────────────────────────
	#[cfg(feature = "quinn")]
	#[error(transparent)]
	QuinnNoInitialCipherSuite(#[from] quinn::crypto::rustls::NoInitialCipherSuite),

	#[cfg(feature = "quinn")]
	#[error(transparent)]
	QuinnConnect(#[from] quinn::ConnectError),

	#[cfg(feature = "quinn")]
	#[error(transparent)]
	QuinnConnection(#[from] quinn::ConnectionError),

	#[cfg(feature = "quinn")]
	#[error(transparent)]
	QuinnClient(#[from] web_transport_quinn::ClientError),

	#[cfg(feature = "quinn")]
	#[error(transparent)]
	QuinnServer(#[from] web_transport_quinn::ServerError),

	#[cfg(feature = "quinn")]
	#[error("failed to build client certificate verifier")]
	ClientVerifier(#[source] rustls::server::VerifierBuilderError),

	#[cfg(feature = "quinn")]
	#[error("failed to establish QUIC connection")]
	QuinnEstablish(#[source] quinn::ConnectionError),

	#[cfg(feature = "quinn")]
	#[error("failed to receive WebTransport request")]
	QuinnRecvRequest(#[source] web_transport_quinn::ServerError),

	// ── noq backend ─────────────────────────────────────────────────────
	#[cfg(feature = "noq")]
	#[error(transparent)]
	NoqNoInitialCipherSuite(#[from] web_transport_noq::noq::crypto::rustls::NoInitialCipherSuite),

	#[cfg(feature = "noq")]
	#[error(transparent)]
	NoqConnect(#[from] web_transport_noq::noq::ConnectError),

	// noq re-exports quinn-proto's `ConnectionError`, so this `#[from]` would
	// collide with quinn's when both backends are enabled. Drop it when quinn
	// is present; quinn's identically-typed variant covers noq's `?` too.
	#[cfg(all(feature = "noq", not(feature = "quinn")))]
	#[error(transparent)]
	NoqConnection(#[from] web_transport_noq::noq::ConnectionError),

	#[cfg(feature = "noq")]
	#[error(transparent)]
	NoqClient(#[from] web_transport_noq::ClientError),

	#[cfg(feature = "noq")]
	#[error(transparent)]
	NoqServer(#[from] web_transport_noq::ServerError),

	#[cfg(feature = "noq")]
	#[error("failed to establish QUIC connection")]
	NoqEstablish(#[source] web_transport_noq::noq::ConnectionError),

	#[cfg(feature = "noq")]
	#[error("failed to receive WebTransport request")]
	NoqRecvRequest(#[source] web_transport_noq::ServerError),

	// ── quiche backend ──────────────────────────────────────────────────
	#[cfg(feature = "quiche")]
	#[error(transparent)]
	QuicheConnection(#[from] web_transport_quiche::ez::ConnectionError),

	#[cfg(feature = "quiche")]
	#[error("fingerprint verification (http:// scheme) is not supported with the quiche backend")]
	QuicheFingerprintUnsupported,

	#[cfg(feature = "quiche")]
	#[error("--tls-cert and --tls-key are required with the quiche backend")]
	QuicheCertRequired,

	#[cfg(feature = "quiche")]
	#[error("must provide matching --tls-cert and --tls-key pairs")]
	QuicheCertPairMismatch,

	#[cfg(feature = "quiche")]
	#[error("failed to connect to quiche server")]
	QuicheConnect(#[source] std::io::Error),

	#[cfg(feature = "quiche")]
	#[error("failed to establish quiche connection")]
	QuicheEstablish(#[source] web_transport_quiche::ez::ConnectionError),

	#[cfg(feature = "quiche")]
	#[error("failed to connect to quiche server")]
	QuicheClientConnect(#[source] web_transport_quiche::ClientError),

	#[cfg(feature = "quiche")]
	#[error("failed to create quiche server")]
	QuicheServerBuild(#[source] std::io::Error),

	#[cfg(feature = "quiche")]
	#[error("failed to accept WebTransport request")]
	QuicheAcceptRequest(#[source] web_transport_quiche::ServerError),

	#[cfg(feature = "quiche")]
	#[error("failed to accept quiche WebTransport")]
	QuicheAccept(#[source] web_transport_quiche::ServerError),

	#[cfg(feature = "quiche")]
	#[error("failed to close quiche WebTransport request")]
	QuicheReject(#[source] web_transport_quiche::ServerError),

	// ── iroh backend ────────────────────────────────────────────────────
	#[cfg(feature = "iroh")]
	#[error("invalid iroh secret key")]
	IrohSecret(#[source] web_transport_iroh::iroh::KeyParsingError),

	#[cfg(feature = "iroh")]
	#[error(transparent)]
	IrohBind(#[from] web_transport_iroh::iroh::endpoint::BindError),

	#[cfg(feature = "iroh")]
	#[error(transparent)]
	IrohBindAddr(#[from] web_transport_iroh::iroh::endpoint::InvalidSocketAddr),

	#[cfg(feature = "iroh")]
	#[error(transparent)]
	IrohConnect(#[from] web_transport_iroh::iroh::endpoint::ConnectWithOptsError),

	#[cfg(feature = "iroh")]
	#[error(transparent)]
	IrohConnecting(#[from] web_transport_iroh::iroh::endpoint::ConnectingError),

	#[cfg(feature = "iroh")]
	#[error(transparent)]
	IrohAlpn(#[from] web_transport_iroh::iroh::endpoint::AlpnError),

	#[cfg(feature = "iroh")]
	#[error(transparent)]
	IrohConnection(#[from] web_transport_iroh::iroh::endpoint::ConnectionError),

	#[cfg(feature = "iroh")]
	#[error(transparent)]
	IrohClient(#[from] web_transport_iroh::ClientError),

	#[cfg(feature = "iroh")]
	#[error(transparent)]
	IrohServer(#[from] web_transport_iroh::ServerError),

	#[cfg(feature = "iroh")]
	#[error("Invalid URL: missing host")]
	MissingHost,

	#[cfg(feature = "iroh")]
	#[error("Invalid URL: host is not an iroh endpoint id")]
	InvalidEndpointId(#[source] web_transport_iroh::iroh::KeyParsingError),

	#[cfg(feature = "iroh")]
	#[error("invalid URL")]
	InvalidUrl,

	#[cfg(feature = "iroh")]
	#[error("failed to receive WebTransport request")]
	IrohRecvRequest(#[source] web_transport_iroh::ServerError),

	// ── WebSocket backend ───────────────────────────────────────────────
	#[cfg(feature = "websocket")]
	#[error("WebSocket support is disabled")]
	WebSocketDisabled,

	#[cfg(feature = "websocket")]
	#[error("missing hostname")]
	MissingHostname,

	#[cfg(feature = "websocket")]
	#[error("unsupported URL scheme for WebSocket: {0}")]
	UnsupportedWebSocketScheme(String),

	#[cfg(feature = "websocket")]
	#[error("failed to connect WebSocket")]
	WebSocketConnect(#[source] qmux::Error),

	#[cfg(feature = "websocket")]
	#[error("WebSocket accept failed")]
	WebSocketAccept(#[source] qmux::Error),

	// ── Reconnect ───────────────────────────────────────────────────────
	#[error("{0}")]
	Reconnect(String),
}

/// Convenience alias for results produced by this crate.
pub type Result<T> = std::result::Result<T, Error>;
