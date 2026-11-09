//! Shared test certificates.

#![allow(dead_code, reason = "shared by several test binaries, each using only part of it")]

/// A self-signed certificate on disk for `localhost` and `127.0.0.1`.
pub struct Certs {
	pub dir: tempfile::TempDir,
	pub cert: std::path::PathBuf,
	pub key: std::path::PathBuf,
}

pub fn certs() -> anyhow::Result<Certs> {
	let signed = (|| -> std::result::Result<_, rcgen::Error> {
		let signing_key = rind_rcgen::KeyPair::generate(&rcgen::PKCS_ECDSA_P256_SHA256)?;
		let mut params = rcgen::CertificateParams::new(vec!["localhost".into(), "127.0.0.1".into()])?;
		params.serial_number = Some(1u64.into());
		let cert = params.self_signed(&signing_key)?;
		Ok(rcgen::CertifiedKey { cert, signing_key })
	})()?;
	let dir = tempfile::tempdir()?;
	let cert = dir.path().join("cert.pem");
	let key = dir.path().join("key.pem");
	std::fs::write(&cert, signed.cert.pem())?;
	std::fs::write(
		&key,
		pem::encode(&pem::Pem::new(
			"PRIVATE KEY",
			signed.signing_key.to_pkcs8().secret_pkcs8_der(),
		)),
	)?;
	Ok(Certs { dir, cert, key })
}

impl Certs {
	/// A server TLS policy presenting this certificate and speaking `alpn`.
	pub fn server(&self, alpn: &[&str]) -> std::sync::Arc<rustls::ServerConfig> {
		let mut tls = moq_tokio::tls::Listen::default();
		tls.cert = vec![self.cert.clone()];
		tls.key = vec![self.key.clone()];
		tls.server_config(alpn.iter().map(|proto| proto.as_bytes().to_vec()).collect())
			.expect("server tls")
	}
}

/// A client TLS policy offering `alpn` and trusting whatever certificate the server presents.
pub fn client_tls(alpn: &[&str]) -> std::sync::Arc<rustls::ClientConfig> {
	let mut tls = moq_tokio::tls::Connect::default();
	tls.insecure = Some(true);
	let mut tls = tls.build().expect("client tls");
	tls.alpn_protocols = alpn.iter().map(|proto| proto.as_bytes().to_vec()).collect();
	std::sync::Arc::new(tls)
}

/// Open a WebTransport session to `request` from tokio, through the driver's
/// own `connect`, trusting whatever certificate the server presents.
pub async fn web_connect(
	request: web_transport_proto::ConnectRequest,
	transport: moq_tokio::quic::driver::transport::Config,
) -> Result<moq_tokio::quic::web::Session, moq_tokio::quic::web::Error> {
	use moq_tokio::quic::{driver, web};

	let host = request.url.host_str().expect("url host").to_string();
	let port = request.url.port().unwrap_or(443);
	let addr = tokio::net::lookup_host((host.as_str(), port))
		.await
		.expect("resolve")
		.next()
		.expect("an address");

	let mut tls = moq_tokio::tls::Connect::default();
	tls.insecure = Some(true);
	let mut crypto = tls.build().expect("tls");
	crypto.alpn_protocols = vec![web::ALPN.as_bytes().to_vec()];
	let mut config = driver::client::Config::new(std::sync::Arc::new(crypto));
	config.transport = transport;

	let bind = match addr {
		std::net::SocketAddr::V4(_) => "0.0.0.0:0",
		std::net::SocketAddr::V6(_) => "[::]:0",
	};
	let socket = std::net::UdpSocket::bind(bind).expect("bind");
	let endpoint = driver::endpoint::Endpoint::new(Default::default(), socket).expect("endpoint");
	let conn = endpoint.connect(&config, addr, &host).expect("dial").await?;
	web::Session::connect(conn, request).await
}
