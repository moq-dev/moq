use crate::client::ClientConfig;
use crate::server::{PeerIdentity, ServerConfig, ServerId, ServerTlsInfo};
use crate::tls::{FingerprintVerifier, ServeCerts};
use anyhow::Context;
use rustls::pki_types::CertificateDer;
use std::sync::{Arc, RwLock};
use std::time::Duration;
use std::{net, time};
use url::Url;

// ── Client ──────────────────────────────────────────────────────────

#[derive(Clone)]
pub(crate) struct QuinnClient {
	pub quic: quinn::Endpoint,
	pub transport: Arc<quinn::TransportConfig>,
	pub versions: moq_lite::Versions,
}

impl QuinnClient {
	pub fn new(config: &ClientConfig) -> anyhow::Result<Self> {
		let socket = std::net::UdpSocket::bind(config.bind).context("failed to bind UDP socket")?;

		// TODO Validate the BBR implementation before enabling it
		let mut transport = quinn::TransportConfig::default();
		transport.max_idle_timeout(Some(time::Duration::from_secs(30).try_into().unwrap()));
		transport.keep_alive_interval(Some(time::Duration::from_secs(5)));
		transport.mtu_discovery_config(None); // Disable MTU discovery

		let max_streams = config.max_streams.unwrap_or(crate::DEFAULT_MAX_STREAMS);
		let max_streams = quinn::VarInt::from_u64(max_streams).unwrap_or(quinn::VarInt::MAX);
		transport.max_concurrent_bidi_streams(max_streams);
		transport.max_concurrent_uni_streams(max_streams);

		let transport = Arc::new(transport);

		// There's a bit more boilerplate to make a generic endpoint.
		let runtime = quinn::default_runtime().context("no async runtime")?;
		let endpoint_config = quinn::EndpointConfig::default();

		// Create the generic QUIC endpoint.
		let quic =
			quinn::Endpoint::new(endpoint_config, None, socket, runtime).context("failed to create QUIC endpoint")?;

		Ok(Self {
			quic,
			transport,
			versions: config.versions(),
		})
	}

	pub async fn connect(&self, tls: &rustls::ClientConfig, url: Url) -> anyhow::Result<web_transport_quinn::Session> {
		let mut url = url;
		let mut config = tls.clone();

		let host = url.host().context("invalid DNS name")?.to_string();
		let port = url.port().unwrap_or(443);

		// Look up the DNS entry.
		// Quinn doesn't support happy eyeballs, so we use the first address.
		let ip = tokio::net::lookup_host((host.clone(), port))
			.await
			.context("failed DNS lookup")?
			.next()
			.context("no DNS entries")?;

		if url.scheme() == "http" {
			// Perform a HTTP request to fetch the certificate fingerprint.
			let mut fingerprint = url.clone();
			fingerprint.set_path("/certificate.sha256");
			fingerprint.set_query(None);
			fingerprint.set_fragment(None);

			tracing::warn!(url = %fingerprint, "performing insecure HTTP request for certificate");

			let resp = reqwest::get(fingerprint.as_str())
				.await
				.context("failed to fetch fingerprint")?
				.error_for_status()
				.context("fingerprint request failed")?;

			let fingerprint = resp.text().await.context("failed to read fingerprint")?;
			let fingerprint = hex::decode(fingerprint.trim()).context("invalid fingerprint")?;

			let verifier = FingerprintVerifier::new(config.crypto_provider().clone(), fingerprint);
			config.dangerous().set_certificate_verifier(Arc::new(verifier));

			url.set_scheme("https").expect("failed to set scheme");
		}

		let alpns: Vec<Vec<u8>> = match url.scheme() {
			"https" => vec![web_transport_quinn::ALPN.as_bytes().to_vec()],
			"moqt" | "moql" => self
				.versions
				.alpns()
				.iter()
				.map(|alpn| alpn.as_bytes().to_vec())
				.collect(),
			_ => anyhow::bail!("url scheme must be 'https', 'moqt', or 'moql'"),
		};

		config.alpn_protocols = alpns;
		config.key_log = Arc::new(rustls::KeyLogFile::new());

		let config: quinn::crypto::rustls::QuicClientConfig = config.try_into()?;
		let mut config = quinn::ClientConfig::new(Arc::new(config));
		config.transport_config(self.transport.clone());

		tracing::debug!(%url, %ip, "connecting");

		let connection = self.quic.connect_with(config, ip, &host)?.await?;
		tracing::Span::current().record("id", connection.stable_id());

		let mut request = web_transport_quinn::proto::ConnectRequest::new(url.clone());
		for alpn in self.versions.alpns() {
			request = request.with_protocol(alpn.to_string());
		}

		let session = match url.scheme() {
			"https" => web_transport_quinn::Session::connect(connection, request).await?,
			"moqt" | "moql" => {
				let handshake = connection
					.handshake_data()
					.context("missing handshake data")?
					.downcast::<quinn::crypto::rustls::HandshakeData>()
					.unwrap();

				let alpn = handshake.protocol.context("missing ALPN")?;
				let alpn = String::from_utf8(alpn).context("failed to decode ALPN")?;

				let response = web_transport_quinn::proto::ConnectResponse::OK.with_protocol(alpn);
				web_transport_quinn::Session::raw(connection, request, response)
			}
			_ => anyhow::bail!("unsupported URL scheme: {}", url.scheme()),
		};

		Ok(session)
	}
}

// ── Peer identity extraction ───────────────────────────────────────

fn extract_peer_identity(conn: &quinn::Connection) -> anyhow::Result<Option<PeerIdentity>> {
	// rustls already validated the client cert chain against our configured
	// `tls.root` during the handshake, so we just need to report whether one
	// was presented. The caller uses that as the gate for cluster-level trust.
	let Some(any) = conn.peer_identity() else {
		return Ok(None);
	};
	any.downcast::<Vec<CertificateDer<'static>>>()
		.map_err(|_| anyhow::anyhow!("peer identity is not a rustls certificate chain"))?;
	Ok(Some(PeerIdentity::default()))
}

// ── Server ──────────────────────────────────────────────────────────

pub(crate) struct QuinnServer {
	pub quic: quinn::Endpoint,
	pub certs: Arc<ServeCerts>,
}

impl QuinnServer {
	pub fn new(config: ServerConfig) -> anyhow::Result<Self> {
		// Enable BBR congestion control
		// TODO Validate the BBR implementation before enabling it
		let mut transport = quinn::TransportConfig::default();
		transport.max_idle_timeout(Some(Duration::from_secs(30).try_into().unwrap()));
		transport.keep_alive_interval(Some(Duration::from_secs(5)));
		transport.mtu_discovery_config(None); // Disable MTU discovery

		let max_streams = config.max_streams.unwrap_or(crate::DEFAULT_MAX_STREAMS);
		let max_streams = quinn::VarInt::from_u64(max_streams).unwrap_or(quinn::VarInt::MAX);
		transport.max_concurrent_bidi_streams(max_streams);
		transport.max_concurrent_uni_streams(max_streams);

		let transport = Arc::new(transport);

		let provider = crate::crypto::provider();

		let certs = ServeCerts::new(provider.clone());
		certs.load_certs(&config.tls)?;
		let certs = Arc::new(certs);

		let tls_builder = rustls::ServerConfig::builder_with_provider(provider.clone())
			.with_protocol_versions(&[&rustls::version::TLS13])?;

		let mut tls = if config.tls.root.is_empty() {
			tls_builder.with_no_client_auth().with_cert_resolver(certs.clone())
		} else {
			let roots = config.tls.load_roots()?;
			let verifier = rustls::server::WebPkiClientVerifier::builder_with_provider(Arc::new(roots), provider)
				.allow_unauthenticated()
				.build()
				.context("failed to build client certificate verifier")?;
			tls_builder
				.with_client_cert_verifier(verifier)
				.with_cert_resolver(certs.clone())
		};

		// H3 is last because it requires WebTransport framing which not all H3 endpoints support.
		let mut alpns: Vec<Vec<u8>> = config
			.versions()
			.alpns()
			.iter()
			.map(|alpn| alpn.as_bytes().to_vec())
			.collect();
		alpns.push(web_transport_quinn::ALPN.as_bytes().to_vec());

		tls.alpn_protocols = alpns;
		tls.key_log = Arc::new(rustls::KeyLogFile::new());

		let tls: quinn::crypto::rustls::QuicServerConfig = tls.try_into()?;
		let mut tls = quinn::ServerConfig::with_crypto(Arc::new(tls));
		tls.transport_config(transport);

		// There's a bit more boilerplate to make a generic endpoint.
		let runtime = quinn::default_runtime().context("no async runtime")?;

		let listen = crate::util::resolve(config.bind.as_deref(), crate::server::DEFAULT_BIND)
			.context("failed to resolve bind address")?;

		// Configure connection ID generator with server ID if provided
		let mut endpoint_config = quinn::EndpointConfig::default();
		if let Some(server_id) = config.quic_lb_id {
			let nonce_len = config.quic_lb_nonce.unwrap_or(8);
			anyhow::ensure!(nonce_len >= 4, "quic_lb_nonce must be at least 4");

			let cid_len = 1 + server_id.len() + nonce_len;
			anyhow::ensure!(cid_len <= 20, "connection ID length ({cid_len}) exceeds maximum of 20");

			tracing::info!(
				?server_id,
				nonce_len,
				"using QUIC-LB compatible connection ID generation"
			);
			endpoint_config.cid_generator(move || Box::new(ServerIdGenerator::new(server_id.clone(), nonce_len)));
		}

		let socket = std::net::UdpSocket::bind(listen).context("failed to bind UDP socket")?;

		// Create the generic QUIC endpoint.
		let quic = quinn::Endpoint::new(endpoint_config, Some(tls), socket, runtime)
			.context("failed to create QUIC endpoint")?;

		// Spawn cert reload listener only after endpoint creation succeeds,
		// so we don't leave a dangling signal listener on failure.
		#[cfg(unix)]
		tokio::spawn(crate::tls::reload_certs(certs.clone(), config.tls.clone()));

		Ok(Self { quic, certs })
	}

	pub fn accept(&self) -> impl std::future::Future<Output = Option<quinn::Incoming>> + '_ {
		self.quic.accept()
	}

	pub fn tls_info(&self) -> Arc<RwLock<ServerTlsInfo>> {
		self.certs.info.clone()
	}

	pub fn local_addr(&self) -> anyhow::Result<net::SocketAddr> {
		self.quic.local_addr().context("failed to get local address")
	}

	pub fn close(&self) {
		self.quic.close(quinn::VarInt::from_u32(0), b"server shutdown");
	}
}

// ── QuinnRequest ────────────────────────────────────────────────────

/// A raw QUIC connection request without WebTransport framing (quinn backend).
pub(crate) enum QuinnRequest {
	Raw {
		request: web_transport_quinn::proto::ConnectRequest,
		response: web_transport_quinn::proto::ConnectResponse,
		connection: quinn::Connection,
	},
	WebTransport {
		request: web_transport_quinn::Request,
		alpns: Vec<&'static str>,
	},
}

impl QuinnRequest {
	pub async fn accept(conn: quinn::Incoming, alpns: Vec<&'static str>) -> anyhow::Result<Self> {
		let mut conn = conn.accept()?;

		let handshake = conn
			.handshake_data()
			.await?
			.downcast::<quinn::crypto::rustls::HandshakeData>()
			.unwrap();

		let alpn = handshake.protocol.context("missing ALPN")?;
		let alpn = String::from_utf8(alpn).context("failed to decode ALPN")?;
		let host = handshake.server_name.unwrap_or_default();

		tracing::debug!(%host, ip = %conn.remote_address(), %alpn, "accepting");

		// Wait for the QUIC connection to be established.
		let conn = conn.await.context("failed to establish QUIC connection")?;

		let span = tracing::Span::current();
		span.record("id", conn.stable_id()); // TODO can we get this earlier?
		tracing::debug!(%host, ip = %conn.remote_address(), %alpn, "accepted");

		match alpn.as_str() {
			web_transport_quinn::ALPN => {
				// Wait for the CONNECT request.
				let request = web_transport_quinn::Request::accept(conn)
					.await
					.context("failed to receive WebTransport request")?;
				Ok(Self::WebTransport { request, alpns })
			}
			alpn if moq_lite::ALPNS.contains(&alpn) => {
				anyhow::ensure!(!host.is_empty(), "missing server name for raw QUIC connection");
				let host_str = if host.contains(':') {
					format!("[{}]", host)
				} else {
					host.clone()
				};
				let url = format!("moqt://{}", host_str)
					.parse::<Url>()
					.context("failed to construct URL from server name")?;
				let request = web_transport_quinn::proto::ConnectRequest::new(url);
				let response = web_transport_quinn::proto::ConnectResponse::OK.with_protocol(alpn);
				Ok(Self::Raw {
					connection: conn,
					request,
					response,
				})
			}
			_ => anyhow::bail!("unsupported ALPN: {alpn}"),
		}
	}

	/// Accept the session, returning a 200 OK if using WebTransport.
	pub async fn ok(self) -> Result<web_transport_quinn::Session, web_transport_quinn::ServerError> {
		match self {
			QuinnRequest::Raw {
				connection,
				request,
				response,
			} => Ok(web_transport_quinn::Session::raw(connection, request, response)),
			QuinnRequest::WebTransport { request, alpns } => {
				let mut response = web_transport_quinn::proto::ConnectResponse::OK;
				// Pick the first sub-protocol that we actually support.
				// This is the WebTransport equivalent of ALPN negotiation.
				// If no match is found, we default to no sub-protocol to support older
				// clients that don't use ALPN. We assume moq-transport-14/moq-lite-02
				// and perform the SETUP_x exchange instead.
				if let Some(protocol) = request.protocols.iter().find(|p| alpns.contains(&p.as_str())) {
					response = response.with_protocol(protocol);
				}
				request.respond(response).await
			}
		}
	}

	/// Returns the URL provided by the client.
	pub fn url(&self) -> Option<&Url> {
		match self {
			QuinnRequest::Raw { .. } => None,
			QuinnRequest::WebTransport { request, .. } => Some(&request.url),
		}
	}

	/// Returns the peer's validated client certificate identity, if any.
	pub fn peer_identity(&self) -> anyhow::Result<Option<PeerIdentity>> {
		let conn = match self {
			QuinnRequest::Raw { connection, .. } => connection,
			QuinnRequest::WebTransport { request, .. } => request.conn(),
		};
		extract_peer_identity(conn)
	}

	/// Reject the session with a status code.
	pub async fn close(
		self,
		status: web_transport_quinn::http::StatusCode,
	) -> Result<(), web_transport_quinn::ServerError> {
		match self {
			QuinnRequest::Raw { connection, .. } => {
				connection.close(status.as_u16().into(), status.as_str().as_bytes());
				Ok(())
			}
			QuinnRequest::WebTransport { request, alpns: _, .. } => request.reject(status).await,
		}
	}
}

// ── ServerIdGenerator ───────────────────────────────────────────────

struct ServerIdGenerator {
	server_id: ServerId,
	nonce_len: usize,
}

impl ServerIdGenerator {
	fn new(server_id: ServerId, nonce_len: usize) -> Self {
		Self { server_id, nonce_len }
	}
}

impl quinn::ConnectionIdGenerator for ServerIdGenerator {
	fn generate_cid(&mut self) -> quinn::ConnectionId {
		use rand::Rng;
		let cid_len = self.cid_len();
		let mut cid = Vec::with_capacity(cid_len);
		// First byte has "self-encoded length" of server ID + nonce
		cid.push((cid_len - 1) as u8);
		cid.extend(self.server_id.0.iter());
		cid.extend(rand::rng().random_iter::<u8>().take(self.nonce_len));
		quinn::ConnectionId::new(cid.as_slice())
	}

	fn cid_len(&self) -> usize {
		1 + self.server_id.len() + self.nonce_len
	}

	fn cid_lifetime(&self) -> Option<Duration> {
		None
	}
}
