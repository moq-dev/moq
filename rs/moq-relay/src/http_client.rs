use anyhow::Context;
use http_cache_reqwest::{Cache, CacheMode, HttpCache, HttpCacheOptions, MokaCache, MokaManager};
use reqwest_middleware::ClientWithMiddleware;
use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr};
use std::time::Duration;

/// How long any single request may take.
pub(crate) const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

/// Build a reqwest client with RFC-compliant HTTP caching (honors `Cache-Control`,
/// `ETag`, `Last-Modified`). The API uses its own server trust while presenting
/// the supplied client identity, so an mTLS-gated endpoint can identify the relay.
///
/// `localhost` is pinned to loopback instead of resolved, since plaintext HTTP
/// is accepted only on loopback.
///
/// Used by the cluster's peer-list polling (`--cluster-api-url`).
pub(crate) fn build(tls: &rustls::ClientConfig, roots: &[std::path::PathBuf]) -> anyhow::Result<ClientWithMiddleware> {
	let client = reqwest::Client::builder()
		.timeout(REQUEST_TIMEOUT)
		.redirect(reqwest::redirect::Policy::none())
		// Port 0 keeps the port from the URL.
		.resolve_to_addrs(
			"localhost",
			&[
				SocketAddr::from((Ipv4Addr::LOCALHOST, 0)),
				SocketAddr::from((Ipv6Addr::LOCALHOST, 0)),
			],
		)
		.use_preconfigured_tls(server_tls(tls, roots)?)
		.build()
		.context("failed to build HTTP client")?;

	Ok(reqwest_middleware::ClientBuilder::new(client)
		.with(Cache(HttpCache {
			mode: CacheMode::Default,
			manager: MokaManager::new(MokaCache::new(100)),
			options: HttpCacheOptions::default(),
		}))
		.build())
}

/// Verify an HTTP server with its own roots, sharing only the dialer's client identity.
pub(crate) fn server_tls(
	tls: &rustls::ClientConfig,
	roots: &[std::path::PathBuf],
) -> anyhow::Result<rustls::ClientConfig> {
	let mut config = moq_tokio::tls::Connect::default();
	config.root = roots.to_vec();
	let mut client = config.build().context("failed to build HTTP server trust")?;
	// The mesh verifier, hostname override and pins do not apply to HTTP servers.
	// Share the identity resolver so certificate reloads reach both clients.
	client.client_auth_cert_resolver = tls.client_auth_cert_resolver.clone();
	Ok(client)
}

#[cfg(test)]
mod tests {
	use super::*;
	use rcgen::{CertificateParams, KeyPair};
	use rustls::pki_types::PrivateKeyDer;
	use std::{path::PathBuf, sync::Arc};
	use tokio::io::{AsyncReadExt, AsyncWriteExt};

	#[tokio::test]
	async fn cluster_api_refuses_redirects() {
		let _ = moq_tokio::crypto::install_default();
		let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
		let addr = listener.local_addr().unwrap();
		let server = async move {
			let (mut stream, _) = listener.accept().await.unwrap();
			let mut data = [0; 4096];
			assert!(stream.read(&mut data).await.unwrap() > 0);
			// A valid peer list in a redirect must not be accepted either.
			stream
				.write_all(
					b"HTTP/1.1 302 Found\r\nLocation: /other\r\nContent-Length: 2\r\nConnection: close\r\n\r\n[]",
				)
				.await
				.unwrap();
		};
		let tls = moq_tokio::tls::Connect::default().build().unwrap();
		let client = build(&tls, &[]).unwrap();
		let response = async { client.get(format!("http://{addr}/")).send().await.unwrap() };
		let (response, ()) = tokio::join!(response, server);
		assert!(response.status().is_redirection());
	}

	#[tokio::test]
	async fn auth_trust_is_independent_from_mesh_verification() {
		let _ = moq_tokio::crypto::install_default();
		let dir = tempfile::tempdir().unwrap();
		let ca = Authority::new("mesh");
		let (cert, key) = ca.leaf("127.0.0.1");
		let root = dir.path().join("mesh.pem");
		std::fs::write(&root, ca.cert.pem()).unwrap();
		let server = rustls::ServerConfig::builder()
			.with_no_client_auth()
			.with_single_cert(
				vec![cert.der().clone()],
				PrivateKeyDer::Pkcs8(key.serialize_der().into()),
			)
			.unwrap();
		for mode in ["insecure", "root", "fingerprint"] {
			let mut mesh = moq_tokio::tls::Connect::default();
			match mode {
				"insecure" => mesh.insecure = Some(true),
				"root" => mesh.root = vec![root.clone()],
				"fingerprint" => {
					mesh.fingerprint = vec![hex::encode(
						aws_lc_rs::digest::digest(&aws_lc_rs::digest::SHA256, cert.der()).as_ref(),
					)]
				}
				_ => unreachable!(),
			}
			for trusted in [false, true] {
				let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
				let auth = crate::auth::Config {
					root: if trusted { vec![root.clone()] } else { Vec::new() },
					url: Some(format!("https://{}/", listener.local_addr().unwrap()).parse().unwrap()),
					..Default::default()
				};
				let auth = auth.init("relay", &mesh, false).unwrap();
				let serve = async {
					let (stream, _) = listener.accept().await.unwrap();
					if let Ok(mut stream) = tokio_rustls::TlsAcceptor::from(Arc::new(server.clone()))
						.accept(stream)
						.await
					{
						read_request(&mut stream).await;
						let body = r#"{"publish":["**"],"subscribe":["**"]}"#;
						let response = format!(
							"HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
							body.len()
						);
						stream.write_all(response.as_bytes()).await.unwrap();
					}
				};
				let admit = auth.admit(auth.request(moq_auth::Transport::Quic, "/"));
				let (admitted, ()) = tokio::join!(admit, serve);
				assert_eq!(admitted.is_ok(), trusted, "mesh {mode}, auth root {trusted}");
			}
		}
	}

	async fn read_request(stream: &mut (impl tokio::io::AsyncRead + Unpin)) {
		let mut request = Vec::new();
		loop {
			let mut data = [0; 4096];
			let count = stream.read(&mut data).await.unwrap();
			assert!(count > 0);
			request.extend_from_slice(&data[..count]);
			let Some(end) = request.windows(4).position(|bytes| bytes == b"\r\n\r\n") else {
				continue;
			};
			let headers = std::str::from_utf8(&request[..end]).unwrap();
			let length: usize = headers
				.lines()
				.filter_map(|line| line.split_once(':'))
				.find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
				.unwrap()
				.1
				.trim()
				.parse()
				.unwrap();
			if request.len() >= end + 4 + length {
				return;
			}
		}
	}

	struct Authority {
		params: CertificateParams,
		key: KeyPair,
		cert: rcgen::Certificate,
	}

	impl Authority {
		fn new(name: &str) -> Self {
			let key = KeyPair::generate().unwrap();
			let mut params = CertificateParams::new(Vec::<String>::new()).unwrap();
			params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
			params.distinguished_name.push(rcgen::DnType::CommonName, name);
			let cert = params.self_signed(&key).unwrap();
			Self { params, key, cert }
		}

		fn leaf(&self, name: &str) -> (rcgen::Certificate, KeyPair) {
			let key = KeyPair::generate().unwrap();
			let params = CertificateParams::new(vec![name.to_string()]).unwrap();
			let cert = params
				.signed_by(&key, &rcgen::Issuer::from_params(&self.params, &self.key))
				.unwrap();
			(cert, key)
		}
	}

	async fn request(
		tls: rustls::ClientConfig,
		server: rustls::ServerConfig,
		host: &str,
	) -> (bool, Option<Vec<rustls::pki_types::CertificateDer<'static>>>) {
		let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
		let addr = listener.local_addr().unwrap();
		let server = async move {
			let (stream, _) = listener.accept().await.unwrap();
			let Ok(mut stream) = tokio_rustls::TlsAcceptor::from(Arc::new(server)).accept(stream).await else {
				return None;
			};
			let peer = stream.get_ref().1.peer_certificates().map(<[_]>::to_vec);
			let mut data = [0; 4096];
			if stream.read(&mut data).await.is_err() {
				return None;
			}
			stream
				.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\n[]")
				.await
				.unwrap();
			peer
		};
		let client = reqwest::Client::builder()
			.no_proxy()
			.resolve(host, addr)
			.use_preconfigured_tls(tls)
			.build()
			.unwrap();
		let client = async {
			client
				.get(format!("https://{host}:{}/", addr.port()))
				.send()
				.await
				.is_ok()
		};
		tokio::join!(client, server)
	}

	#[tokio::test]
	async fn peer_api_trust_is_independent_and_keeps_cluster_identity() {
		let _ = moq_tokio::crypto::install_default();
		let dir = tempfile::tempdir().unwrap();
		let mesh = Authority::new("mesh");
		let api = Authority::new("API");
		let (identity, identity_key) = mesh.leaf("relay.test");
		let (server_cert, server_key) = api.leaf("api.test");
		let write = |name: &str, pem: String| -> PathBuf {
			let path = dir.path().join(name);
			std::fs::write(&path, pem).unwrap();
			path
		};
		let mut config = moq_tokio::tls::Connect::default();
		config.root = vec![write("mesh.pem", mesh.cert.pem())];
		config.cert = Some(write("client.pem", identity.pem()));
		config.key = Some(write("client-key.pem", identity_key.serialize_pem()));
		let mesh_tls = config.build().unwrap();
		let api_root = write("api.pem", api.cert.pem());
		let mut roots = rustls::RootCertStore::empty();
		roots.add(mesh.cert.der().clone()).unwrap();
		let verifier = rustls::server::WebPkiClientVerifier::builder(Arc::new(roots))
			.build()
			.unwrap();
		let server = rustls::ServerConfig::builder()
			.with_client_cert_verifier(verifier)
			.with_single_cert(
				vec![server_cert.der().clone()],
				PrivateKeyDer::Pkcs8(server_key.serialize_der().into()),
			)
			.unwrap();

		// This is the regression: the mesh CA cannot authenticate the API server.
		let tls = server_tls(&mesh_tls, std::slice::from_ref(&api_root)).unwrap();
		let (ok, peer) = request(tls.clone(), server.clone(), "api.test").await;
		assert!(ok, "API CA must be independent from mesh roots");
		assert_eq!(peer.unwrap(), vec![identity.der().clone()]);
		assert!(
			!request(tls, server.clone(), "wrong.test").await.0,
			"API hostname must be verified"
		);
		assert!(
			!request(server_tls(&mesh_tls, &[]).unwrap(), server.clone(), "api.test")
				.await
				.0,
			"default API trust must not include mesh or test roots"
		);
		assert!(
			!request(mesh_tls.clone(), server.clone(), "api.test").await.0,
			"API roots must not modify mesh trust"
		);
		let (mesh_server_cert, mesh_server_key) = mesh.leaf("api.test");
		// Rebuild the same mTLS listener with a mesh-issued server certificate.
		let mut roots = rustls::RootCertStore::empty();
		roots.add(mesh.cert.der().clone()).unwrap();
		let verifier = rustls::server::WebPkiClientVerifier::builder(Arc::new(roots))
			.build()
			.unwrap();
		let mesh_server = rustls::ServerConfig::builder()
			.with_client_cert_verifier(verifier)
			.with_single_cert(
				vec![mesh_server_cert.der().clone()],
				PrivateKeyDer::Pkcs8(mesh_server_key.serialize_der().into()),
			)
			.unwrap();
		assert!(
			!request(server_tls(&mesh_tls, &[]).unwrap(), mesh_server.clone(), "api.test")
				.await
				.0,
			"API default must not inherit mesh CA"
		);
		assert!(
			!request(
				server_tls(&mesh_tls, &[api_root]).unwrap(),
				mesh_server.clone(),
				"api.test"
			)
			.await
			.0,
			"explicit API roots must not inherit mesh CA"
		);
		assert!(
			request(mesh_tls, mesh_server, "api.test").await.0,
			"mesh verification must remain intact"
		);
		assert!(
			build(&config.build().unwrap(), &[dir.path().join("missing.pem")]).is_err(),
			"malformed API roots must fail before polling"
		);
	}
}
