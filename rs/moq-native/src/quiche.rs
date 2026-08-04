//! QUIC backend built on [`web_transport_quiche`], speaking WebTransport over HTTP/3
//! (`https://`) or raw QUIC (`moqt://` / `moql://`).

use crate::client::ClientConfig;
use crate::crypto;
use crate::quic::CongestionControl;
use crate::quic::Resolved;
use crate::server::ServerConfig;
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use std::net;
use std::path::Path;
use std::sync::{Arc, RwLock};
use url::Url;
use web_transport_quiche::proto::ConnectRequest;

/// Re-exported because this module's public API exposes its types. A major
/// `web-transport-quiche` bump is therefore a breaking change for this crate.
pub use web_transport_quiche;

/// Errors specific to the quiche QUIC backend.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
	/// The UDP socket failed to bind, or another I/O operation failed.
	#[error(transparent)]
	Io(#[from] std::io::Error),

	/// The URL had no host to dial and use as the TLS SNI.
	#[error("invalid DNS name")]
	InvalidDnsName,

	/// DNS lookup failed for the URL's host.
	#[error("DNS lookup failed")]
	DnsLookup(#[source] std::io::Error),

	/// DNS lookup succeeded but returned no usable address.
	#[error("no DNS entries found")]
	NoDnsEntries,

	#[doc(hidden)]
	#[deprecated(note = "fingerprint verification over http:// is now supported; this is never returned")]
	#[error("fingerprint verification (http:// scheme) is not supported with the quiche backend")]
	FingerprintUnsupported,

	/// The `http://` fingerprint bootstrap could not reach the relay.
	#[error("failed to fetch certificate fingerprint")]
	FetchFingerprint(#[source] reqwest::Error),

	/// The relay answered the fingerprint bootstrap with a non-success status.
	#[error("certificate fingerprint request failed")]
	FingerprintStatus(#[source] reqwest::Error),

	/// The fingerprint response body could not be read.
	#[error("failed to read certificate fingerprint")]
	ReadFingerprint(#[source] reqwest::Error),

	/// The fetched fingerprint was not valid hex.
	#[error("invalid certificate fingerprint")]
	InvalidFingerprint(#[source] hex::FromHexError),

	/// The fetched fingerprint decoded to the wrong length, so it isn't a SHA-256.
	#[error("certificate fingerprint must be 32 bytes (SHA-256), got {0}")]
	FingerprintLength(usize),

	/// The URL scheme is one this backend cannot dial.
	#[error("url scheme must be 'https', 'moqt', or 'moql'")]
	InvalidScheme,

	#[doc(hidden)]
	#[deprecated(note = "TLS hostname overrides are now supported; this is never returned")]
	#[error("client tls host_name override is not supported with the quiche backend")]
	HostNameUnsupported,

	#[doc(hidden)]
	#[deprecated(note = "GSO can now be disabled; this is never returned")]
	#[error("the quiche backend cannot disable GSO; drop --*-quic-gso=false or use the quinn backend")]
	GsoUnsupported,

	/// The handshake completed without negotiating an ALPN, so there is no protocol to speak.
	#[error("missing ALPN")]
	MissingAlpn,

	/// The negotiated ALPN was not valid UTF-8.
	#[error("failed to decode ALPN")]
	DecodeAlpn(#[from] std::str::Utf8Error),

	/// The peer negotiated an ALPN this server does not serve.
	#[error("unsupported ALPN: {0}")]
	UnsupportedAlpn(String),

	/// The server's configured bind address could not be resolved.
	#[error("failed to resolve bind address")]
	ResolveBind(#[source] std::io::Error),

	/// The server is not bound to any address, so there is no local address to report.
	#[error("failed to get local address")]
	NoLocalAddr,

	/// The server was given neither a certificate pair nor hostnames to generate one from.
	#[error("--tls-cert and --tls-key are required with the quiche backend")]
	CertRequired,

	/// The server was given a different number of certificates than keys.
	#[error("must provide matching --tls-cert and --tls-key pairs")]
	CertPairMismatch,

	/// The QUIC connection could not be started, usually a bad address or an unusable socket.
	#[error("failed to connect to quiche server")]
	Connect(#[source] std::io::Error),

	/// An established connection failed, including when accepting an incoming one.
	#[error(transparent)]
	Connection(#[from] web_transport_quiche::ez::ConnectionError),

	/// The QUIC handshake failed, most often TLS verification or a timeout.
	#[error("failed to establish quiche connection")]
	Establish(#[source] web_transport_quiche::ez::ConnectionError),

	/// The WebTransport CONNECT failed over an otherwise healthy QUIC connection.
	#[error("failed to connect to quiche server")]
	ClientConnect(#[from] web_transport_quiche::ClientError),

	/// The server refused the WebTransport CONNECT with a recognized status, such as
	/// an auth failure. See [`crate::ConnectError`].
	#[error(transparent)]
	ConnectRejected(#[from] crate::ConnectError),

	/// The server could not bind its socket or load the supplied certificate.
	#[error("failed to create quiche server")]
	ServerBuild(#[source] std::io::Error),

	/// The client never sent a usable WebTransport CONNECT request.
	#[error("failed to accept WebTransport request")]
	AcceptRequest(#[source] web_transport_quiche::ServerError),

	/// The `200 OK` response to a WebTransport CONNECT could not be sent.
	#[error("failed to accept quiche WebTransport")]
	Accept(#[source] web_transport_quiche::ServerError),

	/// The rejection response to a WebTransport CONNECT could not be sent.
	#[error("failed to close quiche WebTransport request")]
	Reject(#[source] web_transport_quiche::ServerError),

	/// The TLS configuration was invalid. See [`crate::tls::Error`].
	#[error(transparent)]
	Tls(#[from] crate::tls::Error),

	/// Two or more addresses were raced and every attempt failed, each paired
	/// with its own error in dial order. All of them are kept: picking one to
	/// report would bury a rejected certificate or a refused port behind
	/// whichever address happened to be unroutable or to blackhole until its
	/// timeout. A host with a single address reports that error directly instead.
	#[error("all {} connection attempts failed: {}", .0.len(), crate::failover::describe(.0))]
	AllAttemptsFailed(Vec<crate::AddressFailure<Error>>),
}

impl crate::failover::Aggregate for Error {
	fn aggregate(failures: Vec<crate::AddressFailure<Self>>) -> Self {
		Self::AllAttemptsFailed(failures)
	}
}

type Result<T> = std::result::Result<T, Error>;

/// Apply the resolved QUIC settings that live in quiche's transport configuration.
///
/// Keep-alive and GSO are socket-driver settings, so they are applied to the
/// client/server builders instead.
fn apply_settings(settings: &mut web_transport_quiche::Settings, quic: &Resolved) -> Result<()> {
	settings.initial_max_streams_bidi = quic.max_streams;
	settings.initial_max_streams_uni = quic.max_streams;
	settings.max_idle_timeout = Some(quic.idle_timeout);
	settings.discover_path_mtu = quic.mtu_discovery;

	// Live media wants a steady send rate an encoder can track, not CUBIC's sawtooth,
	// so default to BBR rather than quiche's own CUBIC.
	let family = quic.congestion_control.unwrap_or(CongestionControl::Delay);
	settings.cc_algorithm = cc_algorithm(family).to_owned();

	// quiche writes one file per connection itself, named after the connection ID.
	if let Some(dir) = quic.qlog_dir() {
		settings.qlog_dir = Some(dir.to_string_lossy().into_owned());
		tracing::info!(dir = %dir.display(), "writing qlog");
	}

	Ok(())
}

/// The quiche controller name for a congestion control family. quiche's BBR is the
/// v2 gcongestion port, and it takes the algorithm by name rather than by type.
fn cc_algorithm(family: CongestionControl) -> &'static str {
	match family {
		CongestionControl::Loss => "cubic",
		CongestionControl::Delay => "bbr2_gcongestion",
	}
}

// ── Client ──────────────────────────────────────────────────────────

#[derive(Clone)]
pub(crate) struct QuicheClient {
	pub bind: net::SocketAddr,
	/// Resolved server-verification policy, shared with the other backends.
	pub verification: crate::tls::Verification,
	/// Whether an `http://` URL may bootstrap a pin (see [crate::tls::Client::allows_http_bootstrap]).
	pub http_bootstrap: bool,
	pub quic: Resolved,
	pub host_name: Option<String>,
	/// Stagger between Happy Eyeballs connection attempts (see [`crate::failover`]).
	pub failover_delay: std::time::Duration,
	identity: Option<Arc<ClientIdentity>>,
}

struct ClientIdentity {
	chain: Vec<CertificateDer<'static>>,
	key: PrivateKeyDer<'static>,
}

impl QuicheClient {
	pub fn new(config: &ClientConfig) -> Result<Self> {
		let quic = config.quic.resolve();
		let identity = match (&config.tls.cert, &config.tls.key) {
			(Some(cert), Some(key)) => {
				let (chain, key) = load_quiche_cert(cert, key)?;
				Some(Arc::new(ClientIdentity { chain, key }))
			}
			(None, None) => None,
			_ => return Err(crate::tls::Error::IncompleteClientAuth.into()),
		};

		// Warn once here rather than on every dial: the constraint is a property of
		// the config, and a relay reconnecting to its peers would repeat it forever.
		if config.bind.port() != 0 {
			tracing::warn!(
				bind = %config.bind,
				"pinned client bind port; only the first resolved address is dialed (address failover needs an ephemeral port on this backend)"
			);
		}

		Ok(Self {
			bind: config.bind,
			verification: config.tls.verification()?,
			http_bootstrap: config.tls.allows_http_bootstrap(),
			quic,
			host_name: config.tls.host_name.clone(),
			failover_delay: config.failover_delay.unwrap_or(crate::failover::DEFAULT_DELAY),
			identity,
		})
	}

	pub async fn connect(&self, url: Url, versions: &moq_net::Versions) -> Result<web_transport_quiche::Connection> {
		use crate::tls::Verification;

		let host = url.host().ok_or(Error::InvalidDnsName)?.to_string();
		let port = url.port().unwrap_or(443);

		// `http://` fetches the relay's self-signed certificate fingerprint over
		// an insecure request and pins it for this connection. It is only honored
		// when no stronger verification is configured: an attacker who controls
		// the plaintext fetch must not be able to weaken an explicit pin or
		// re-enable verification we were told to skip.
		let (url, verification) = if url.scheme() == "http" {
			let mut https = url.clone();
			https.set_scheme("https").expect("https is a valid scheme");

			if self.http_bootstrap {
				let pin = fetch_fingerprint(&url).await?;
				(https, Verification::Fingerprints(vec![pin]))
			} else {
				tracing::warn!(
					"ignoring insecure http:// fingerprint bootstrap; using the configured TLS verification"
				);
				(https, self.verification.clone())
			}
		} else {
			(url, self.verification.clone())
		};

		let alpns: Vec<Vec<u8>> = match url.scheme() {
			"https" => vec![web_transport_quiche::ALPN.as_bytes().to_vec()],
			"moqt" | "moql" => versions.alpns().iter().map(|alpn| alpn.as_bytes().to_vec()).collect(),
			_ => return Err(Error::InvalidScheme),
		};

		// Resolve the trust anchors once: loading the native root store can hit the
		// disk, which shouldn't repeat for every raced address.
		//
		// quiche/boringssl takes a concrete root list rather than a rustls
		// verifier, so the platform verifier the other backends use isn't
		// available here. Trust the native store for system roots; on
		// platforms without one (iOS/Android) this yields nothing and the
		// handshake fails closed.
		let roots = match &verification {
			Verification::Roots { custom, system } => {
				let mut roots = custom.clone();
				if *system {
					let native = rustls_native_certs::load_native_certs();
					for err in native.errors {
						tracing::warn!(%err, "failed to load native root cert");
					}
					roots.extend(native.certs);
				}
				roots
			}
			_ => Vec::new(),
		};

		// Bind the first attempt now so candidate selection uses the socket's actual
		// family and dual-stack state. Later attempts bind their own ephemeral
		// sockets because each quiche connection must own its socket.
		let socket = crate::bind::udp(self.bind)?;
		let local = socket.local_addr()?;
		let dual_stack = crate::bind::udp_is_dual_stack(&socket);

		// Resolve every DNS entry, adapted to the bound socket's reachability; the dial
		// below races them Happy Eyeballs style so one broken family can't stall
		// the connect.
		let addrs = tokio::net::lookup_host((host.as_str(), port))
			.await
			.map_err(Error::DnsLookup)?;
		let mut candidates = crate::failover::match_local(addrs, local, dual_stack);
		if candidates.is_empty() {
			return Err(Error::NoDnsEntries);
		}

		// Each attempt binds its own socket, and a pinned non-zero source port only
		// fits one socket at a time: an overlapping attempt would fail its bind with
		// AddrInUse and burn a candidate for nothing. The attempts can't share a
		// socket either (each quiche instance owns its socket and would consume the
		// other's packets), so keep the preferred candidate only, the same single
		// dial as before failover existed. `new` warns about this once.
		if self.bind.port() != 0 {
			candidates.truncate(1);
		}

		tracing::debug!(%url, ?candidates, "connecting via quiche");

		// Race only the QUIC handshake: the winner alone performs the WebTransport
		// CONNECT below, so the server sees a single request no matter how many
		// addresses were dialed.
		let mut first_socket = Some(socket);
		let conn = crate::failover::race(candidates, self.failover_delay, |addr| {
			let socket = first_socket.take();
			let this = self.clone();
			let alpns = alpns.clone();
			let verification = verification.clone();
			let roots = roots.clone();
			let host = host.clone();
			async move {
				let socket = match socket {
					Some(socket) => socket,
					None => crate::bind::udp(this.bind)?,
				};
				let builder = this.builder(socket, alpns, &verification, roots, &host)?;
				let conn = builder
					.connect(&addr.ip().to_string(), addr.port())
					.await
					.map_err(Error::Connect)?
					.established()
					.await
					.map_err(Error::Establish)?;
				Ok::<_, Error>(conn)
			}
		})
		.await?;

		let mut request = web_transport_quiche::proto::ConnectRequest::new(url.clone());
		for alpn in versions.alpns() {
			request = request.with_protocol(alpn.to_string());
		}

		match url.scheme() {
			"https" => {
				// WebTransport over HTTP/3
				let session = web_transport_quiche::Connection::connect(conn, request)
					.await
					.map_err(map_client_error)?;
				Ok(session)
			}
			"moqt" | "moql" => {
				// Raw QUIC mode
				let alpn = conn.alpn().ok_or(Error::MissingAlpn)?;
				let alpn = std::str::from_utf8(&alpn)?;

				let response = web_transport_quiche::proto::ConnectResponse::OK.with_protocol(alpn);
				Ok(web_transport_quiche::Connection::raw(conn, request, response))
			}
			_ => unreachable!("unsupported URL scheme: {}", url.scheme()),
		}
	}

	/// Assemble the per-attempt client builder: settings, its bound socket, SNI,
	/// client identity, and the verification policy
	/// (with `roots` already resolved by the caller).
	fn builder(
		&self,
		socket: net::UdpSocket,
		alpns: Vec<Vec<u8>>,
		verification: &crate::tls::Verification,
		roots: Vec<CertificateDer<'static>>,
		host: &str,
	) -> Result<web_transport_quiche::ez::ClientBuilder> {
		use crate::tls::Verification;

		let mut settings = web_transport_quiche::Settings::default();
		settings.verify_peer = !matches!(verification, Verification::Disabled);
		settings.alpn = alpns;
		apply_settings(&mut settings, &self.quic)?;

		let mut builder = web_transport_quiche::ez::ClientBuilder::default()
			.with_settings(settings)
			.with_gso(self.quic.gso.unwrap_or(true))
			.with_socket(socket)?;

		if let Some(interval) = self.quic.keep_alive {
			builder = builder.with_keep_alive(interval);
		}

		// The builder dials a resolved IP, so always set the original hostname
		// explicitly for SNI and verification unless the caller overrides it.
		builder = builder.with_server_name(self.host_name.as_deref().unwrap_or(host));

		if let Some(identity) = &self.identity {
			builder = builder.with_single_cert(identity.chain.clone(), identity.key.clone_key());
		}

		match verification {
			// No hook: tokio-quiche's default config with verify_peer = false.
			Verification::Disabled => {}
			Verification::Fingerprints(hashes) => {
				builder = builder.with_server_certificate_hashes(hashes.clone());
			}
			Verification::Roots { .. } => {
				if !roots.is_empty() {
					builder = builder.with_root_certificates(roots);
				}
			}
		}

		Ok(builder)
	}
}

/// Fetch a relay's certificate SHA-256 over an insecure `http://` request.
///
/// This is the native equivalent of how a browser bootstraps trust for a
/// self-signed relay: GET `/certificate.sha256` and pin the returned hash.
async fn fetch_fingerprint(url: &Url) -> Result<[u8; 32]> {
	let mut fp = url.clone();
	fp.set_path("/certificate.sha256");
	fp.set_query(None);
	fp.set_fragment(None);

	tracing::warn!(url = %fp, "performing insecure HTTP request for certificate fingerprint");

	let resp = reqwest::get(fp.as_str())
		.await
		.map_err(Error::FetchFingerprint)?
		.error_for_status()
		.map_err(Error::FingerprintStatus)?;
	let text = resp.text().await.map_err(Error::ReadFingerprint)?;
	let bytes = hex::decode(text.trim()).map_err(Error::InvalidFingerprint)?;
	bytes.try_into().map_err(|v: Vec<u8>| Error::FingerprintLength(v.len()))
}

impl Error {
	pub(crate) fn connect_error(&self) -> Option<crate::ConnectError> {
		match self {
			Self::ConnectRejected(err) => Some(*err),
			Self::ClientConnect(err) => classify_client_error(err),
			Self::AllAttemptsFailed(failures) => failures.iter().find_map(|failure| failure.error.connect_error()),
			_ => None,
		}
	}
}

fn map_client_error(err: web_transport_quiche::ClientError) -> Error {
	if let Some(err) = classify_client_error(&err) {
		return err.into();
	}

	err.into()
}

fn classify_client_error(err: &web_transport_quiche::ClientError) -> Option<crate::ConnectError> {
	match err {
		web_transport_quiche::ClientError::Connect(err) => classify_connect_error(err),
		_ => None,
	}
}

fn classify_connect_error(err: &web_transport_quiche::h3::ConnectError) -> Option<crate::ConnectError> {
	match err {
		web_transport_quiche::h3::ConnectError::Status(status) => crate::ConnectError::from_status_u16(status.as_u16()),
		web_transport_quiche::h3::ConnectError::Proto(err) => classify_proto_error(err),
		_ => None,
	}
}

fn classify_proto_error(err: &web_transport_quiche::proto::ConnectError) -> Option<crate::ConnectError> {
	match err {
		web_transport_quiche::proto::ConnectError::ErrorStatus(status)
		| web_transport_quiche::proto::ConnectError::WrongStatus(Some(status)) => {
			crate::ConnectError::from_status_u16(status.as_u16())
		}
		_ => None,
	}
}

// ── Server ──────────────────────────────────────────────────────────

pub(crate) struct QuicheServer {
	pub server: web_transport_quiche::ez::Server,
	pub certs: crate::tls::Certificates,
}

impl QuicheServer {
	pub fn new(config: ServerConfig) -> Result<Self> {
		if config.quic.quic_lb_id.is_some() {
			tracing::warn!("QUIC-LB is not supported with the quiche backend; ignoring server ID");
		}

		let quic = config.quic.resolve();

		let listen =
			crate::util::resolve(config.bind.as_deref(), crate::server::DEFAULT_BIND).map_err(Error::ResolveBind)?;
		let socket = crate::bind::udp(listen)?;

		let (chain, key) = if !config.tls.generate.is_empty() {
			generate_quiche_cert(&config.tls.generate)?
		} else {
			if config.tls.cert.is_empty() || config.tls.key.is_empty() {
				return Err(Error::CertRequired);
			}
			if config.tls.cert.len() != config.tls.key.len() {
				return Err(Error::CertPairMismatch);
			}

			// Load certs in PEM format and convert to DER for quiche
			load_quiche_cert(&config.tls.cert[0], &config.tls.key[0])?
		};

		// Compute fingerprints using rustls crypto (always available)
		let provider = crypto::provider();
		let fingerprints: Vec<String> = chain
			.iter()
			.map(|cert| hex::encode(crypto::sha256(&provider, cert.as_ref())))
			.collect();

		let certs = crate::tls::Certificates::new(Arc::new(RwLock::new(crate::tls::Info {
			#[cfg(any(feature = "noq", feature = "quinn", feature = "quiche"))]
			certs: Vec::new(),
			fingerprints,
		})));

		// H3 is last because it requires WebTransport framing which not all H3 endpoints support.
		let mut alpns: Vec<Vec<u8>> = config
			.versions()
			.alpns()
			.iter()
			.map(|alpn| alpn.as_bytes().to_vec())
			.collect();
		alpns.push(b"h3".to_vec());

		let mut settings = web_transport_quiche::Settings::default();
		settings.alpn = alpns;
		apply_settings(&mut settings, &quic)?;

		let mut builder = web_transport_quiche::ez::ServerBuilder::default()
			.with_settings(settings)
			.with_gso(quic.gso.unwrap_or(true));

		if let Some(interval) = quic.keep_alive {
			builder = builder.with_keep_alive(interval);
		}

		if !config.tls.root.is_empty() {
			let roots = config
				.tls
				.root
				.iter()
				.map(|path| crate::tls::read_certs(path))
				.collect::<crate::tls::Result<Vec<_>>>()?
				.into_iter()
				.flatten()
				.collect();
			builder = builder.with_client_auth(web_transport_quiche::ClientAuth::Optional(roots));
		}

		let server = builder
			.with_socket(socket)?
			.with_single_cert(chain, key)
			.map_err(Error::ServerBuild)?;

		Ok(Self { server, certs })
	}

	pub fn accept(&mut self) -> impl std::future::Future<Output = Option<web_transport_quiche::ez::Incoming>> + '_ {
		self.server.accept()
	}

	pub fn certificates(&self) -> crate::tls::Certificates {
		self.certs.clone()
	}

	pub fn local_addr(&self) -> Result<net::SocketAddr> {
		self.server.local_addrs().first().copied().ok_or(Error::NoLocalAddr)
	}

	pub fn close(&mut self) {
		// quiche server doesn't have a close method; dropping it is sufficient
	}
}

fn load_quiche_cert(
	cert_path: &Path,
	key_path: &Path,
) -> crate::tls::Result<(Vec<CertificateDer<'static>>, PrivateKeyDer<'static>)> {
	let chain = crate::tls::read_certs(cert_path)?;
	if chain.is_empty() {
		return Err(crate::tls::Error::Empty);
	}

	let key = PrivateKeyDer::from_pem_file(key_path).map_err(crate::tls::Error::Key)?;

	Ok((chain, key))
}

#[cfg(any(feature = "aws-lc-rs", feature = "ring"))]
fn generate_quiche_cert(
	hostnames: &[String],
) -> crate::tls::Result<(Vec<CertificateDer<'static>>, PrivateKeyDer<'static>)> {
	let key_pair = rcgen::KeyPair::generate()?;

	let mut params = rcgen::CertificateParams::new(hostnames)?;

	// Make the certificate valid for two weeks, starting yesterday (in case of clock drift).
	// WebTransport certificates MUST be valid for two weeks at most.
	params.not_before = ::time::OffsetDateTime::now_utc() - ::time::Duration::days(1);
	params.not_after = params.not_before + ::time::Duration::days(14);

	let cert = params.self_signed(&key_pair)?;

	let key_der = key_pair.serialized_der().to_vec();
	let key = PrivateKeyDer::Pkcs8(key_der.into());

	Ok((vec![cert.into()], key))
}

#[cfg(not(any(feature = "aws-lc-rs", feature = "ring")))]
fn generate_quiche_cert(
	hostnames: &[String],
) -> crate::tls::Result<(Vec<CertificateDer<'static>>, PrivateKeyDer<'static>)> {
	Err(crate::tls::Error::NoCryptoProvider)
}

// ── QuicheQuicRequest ───────────────────────────────────────────────

/// A raw QUIC connection request via the quiche backend (not using HTTP/3).
/// Accept a quiche QUIC connection, negotiate WebTransport or raw moq, and complete the
/// handshake (a `200 OK` for WebTransport). Returns the established connection plus the
/// request URL and any validated client identity. Raw QUIC carries no request URL
/// because the path rides the SETUP instead.
pub(crate) async fn accept(
	incoming: web_transport_quiche::ez::Incoming,
	alpns: Vec<&'static str>,
) -> Result<(
	web_transport_quiche::Connection,
	Option<Url>,
	Option<crate::tls::PeerIdentity>,
)> {
	tracing::debug!(ip = %incoming.peer_addr(), "accepting via quiche");

	// Accept the connection and wait for it to be established
	let conn = incoming.accept().await?;

	// Get the negotiated ALPN from the established connection
	let alpn = conn.alpn().ok_or(Error::MissingAlpn)?;
	let alpn = std::str::from_utf8(&alpn)?;
	let identity = conn.peer_certificates().map(crate::tls::PeerIdentity::from_chain);
	tracing::debug!(ip = %conn.peer_addr(), ?alpn, "accepted via quiche");

	match alpn {
		web_transport_quiche::ALPN => {
			// WebTransport over HTTP/3
			let request = web_transport_quiche::h3::Request::accept(conn)
				.await
				.map_err(Error::AcceptRequest)?;
			let url = Some(request.url.clone());

			let mut response = web_transport_quiche::proto::ConnectResponse::OK;
			// Pick the first sub-protocol that we actually support.
			// This is the WebTransport equivalent of ALPN negotiation.
			if let Some(protocol) = request.protocols.iter().find(|p| alpns.contains(&p.as_str())) {
				response = response.with_protocol(protocol);
			}
			let session = request.respond(response).await.map_err(Error::Accept)?;
			Ok((session, url, identity))
		}
		// Recognize any moq ALPN this server actually offered (its configured versions),
		// not the global default set, so opt-in / work-in-progress versions (e.g.
		// moq-lite-06-wip) that are deliberately absent from `moq_net::ALPNS` still work.
		alpn if alpns.contains(&alpn) => {
			let request = ConnectRequest::new("moqt://".to_string().parse::<Url>().unwrap());
			let response = web_transport_quiche::proto::ConnectResponse::OK.with_protocol(alpn);
			// Raw QUIC carries no request URL; the path rides the SETUP.
			let session = web_transport_quiche::Connection::raw(conn, request, response);
			Ok((session, None, identity))
		}
		_ => Err(Error::UnsupportedAlpn(alpn.to_string())),
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	/// quiche takes its controller by name, so a typo here is only caught when a
	/// connection is built. Pin the strings against the set quiche's
	/// `CongestionControlAlgorithm::from_str` accepts.
	#[test]
	fn cc_algorithm_names_are_valid() {
		assert_eq!(cc_algorithm(CongestionControl::Loss), "cubic");
		assert_eq!(cc_algorithm(CongestionControl::Delay), "bbr2_gcongestion");
	}

	/// A selected family must reach the settings quiche is built from, and an unset
	/// one must land on BBR rather than quiche's own CUBIC default.
	#[test]
	fn apply_settings_writes_cc_algorithm() {
		let mut quic = crate::quic::Client::default();
		let mut settings = web_transport_quiche::Settings::default();

		apply_settings(&mut settings, &quic.resolve()).unwrap();
		assert_eq!(settings.cc_algorithm, "bbr2_gcongestion");

		quic.congestion_control = Some(CongestionControl::Loss);
		apply_settings(&mut settings, &quic.resolve()).unwrap();
		assert_eq!(settings.cc_algorithm, "cubic");
	}
}
