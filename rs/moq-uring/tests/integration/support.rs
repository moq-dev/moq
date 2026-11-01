//! Shared test certificates.

#![allow(dead_code, reason = "shared by several test binaries, each using only part of it")]

/// A self-signed certificate on disk for `localhost` and `127.0.0.1`.
pub struct Certs {
	pub dir: tempfile::TempDir,
	pub cert: std::path::PathBuf,
	pub key: std::path::PathBuf,
}

pub fn certs() -> anyhow::Result<Certs> {
	let signed = rcgen::generate_simple_self_signed(vec!["localhost".into(), "127.0.0.1".into()])?;
	let dir = tempfile::tempdir()?;
	let cert = dir.path().join("cert.pem");
	let key = dir.path().join("key.pem");
	std::fs::write(&cert, signed.cert.pem())?;
	std::fs::write(&key, signed.signing_key.serialize_pem())?;
	Ok(Certs { dir, cert, key })
}

/// A CA-signed server certificate plus a root file holding two CAs, only the
/// second of which signed it.
pub struct Bundle {
	pub dir: tempfile::TempDir,
	pub cert: std::path::PathBuf,
	pub key: std::path::PathBuf,
	/// Both CAs concatenated, in that order.
	pub roots: std::path::PathBuf,
}

/// Build one, for a test that a root file is loaded whole rather than to its
/// first certificate.
pub fn bundle() -> anyhow::Result<Bundle> {
	let ca = |name: &str| -> anyhow::Result<rcgen::CertifiedIssuer<'static, rcgen::KeyPair>> {
		let key = rcgen::KeyPair::generate()?;
		let mut params = rcgen::CertificateParams::new(Vec::new())?;
		params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
		params.key_usages = vec![rcgen::KeyUsagePurpose::KeyCertSign, rcgen::KeyUsagePurpose::CrlSign];
		params.distinguished_name.push(rcgen::DnType::CommonName, name);
		Ok(rcgen::CertifiedIssuer::self_signed(params, key)?)
	};
	let unrelated = ca("moq unrelated ca")?;
	let signer = ca("moq signing ca")?;

	let key = rcgen::KeyPair::generate()?;
	let mut params = rcgen::CertificateParams::new(vec!["localhost".into()])?;
	params.use_authority_key_identifier_extension = true;
	let cert = params.signed_by(&key, &signer)?;

	let dir = tempfile::tempdir()?;
	let cert_path = dir.path().join("cert.pem");
	let key_path = dir.path().join("key.pem");
	let roots_path = dir.path().join("roots.pem");
	std::fs::write(&cert_path, cert.pem())?;
	std::fs::write(&key_path, key.serialize_pem())?;
	// The signer is second on purpose: reading only the first certificate
	// leaves the store unable to verify anything this server presents.
	std::fs::write(&roots_path, format!("{}{}", unrelated.pem(), signer.pem()))?;
	Ok(Bundle {
		dir,
		cert: cert_path,
		key: key_path,
		roots: roots_path,
	})
}

/// Open a WebTransport session to `request` from tokio, through the driver's
/// own `connect`, trusting whatever certificate the server presents.
pub async fn web_connect(
	request: moq_tokio::quic::web::proto::ConnectRequest,
	transport: moq_tokio::quic::driver::transport::Config,
) -> Result<moq_tokio::quic::web::Session, moq_tokio::quic::web::ClientError> {
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
	let crypto = driver::crypto::rustls::QuicClientConfig::try_from(crypto).expect("quic crypto");
	let mut config = driver::client::Config::new(std::sync::Arc::new(crypto));
	config.transport = transport;

	let bind = match addr {
		std::net::SocketAddr::V4(_) => "0.0.0.0:0",
		std::net::SocketAddr::V6(_) => "[::]:0",
	};
	let socket = std::net::UdpSocket::bind(bind).expect("bind");
	let endpoint = driver::endpoint::Endpoint::new(Default::default(), socket).expect("endpoint");
	let conn = endpoint.connect(&config, addr, &host)?.await?;
	web::Session::connect(conn, request).await
}
