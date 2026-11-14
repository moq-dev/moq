//! An endpoint for the [QUIC Interop Runner](https://github.com/quic-interop/quic-interop-runner),
//! serving and fetching files over `hq-interop` (HTTP/0.9) on [`moq_tokio::quic::driver`].
//!
//! The runner's container sets `ROLE`, `TESTCASE`, `REQUESTS`, and `SSLKEYLOGFILE`, mounts the
//! runner's certificates at `/certs`, the files to serve at `/www`, and the download target at
//! `/downloads`. A case this endpoint does not support exits 127, which the runner reports as
//! unsupported; `test/quic-interop/cases.toml` says why for each one.

use std::{
	net::{Ipv6Addr, SocketAddr, UdpSocket},
	path::{Component, Path, PathBuf},
	process::ExitCode,
	sync::Arc,
};

use anyhow::{Context, bail, ensure};
use moq_tokio::quic::driver::{
	VarInt, client,
	connection::Connection,
	endpoint::{self, Endpoint, Incoming},
	recv, send, server,
};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, pem::PemObject};
use tokio::io::AsyncReadExt;

const ALPN: &[u8] = b"hq-interop";
/// A request line is a method and a path; nothing legitimate comes close to this.
const MAX_REQUEST: usize = 4096;
/// How much of a file is read into memory per write.
const CHUNK: usize = 64 * 1024;
/// The exit code the runner reads as "test case not supported".
const UNSUPPORTED: u8 = 127;

/// What a client does with the runner's requests.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Fetch {
	/// Every request on one connection, in parallel.
	One,
	/// The first request on one connection, the rest on a second that resumes its TLS session.
	Resume,
	/// Each request on its own connection, one after another.
	Each,
}

/// What a server does with each incoming connection.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Accept {
	/// Accept at once.
	Direct,
	/// Send a Retry first, unless the client already carries a token proving its address.
	Retry,
}

fn main() -> ExitCode {
	tracing_subscriber::fmt()
		.with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
		.with_writer(std::io::stderr)
		.init();

	let role = std::env::var("ROLE").unwrap_or_default();
	let case = std::env::var("TESTCASE").unwrap_or_default();
	let runtime = tokio::runtime::Runtime::new().expect("tokio runtime");
	let result = match role.as_str() {
		"server" => match server_case(&case) {
			Some(accept) => runtime.block_on(serve(accept)),
			None => return unsupported(&role, &case),
		},
		"client" => match client_case(&case) {
			Some(fetch) => runtime.block_on(fetch_all(fetch)),
			None => return unsupported(&role, &case),
		},
		_ => Err(anyhow::anyhow!("ROLE must be client or server, not {role:?}")),
	};
	match result {
		Ok(()) => ExitCode::SUCCESS,
		Err(err) => {
			tracing::error!("{err:#}");
			ExitCode::FAILURE
		}
	}
}

fn unsupported(role: &str, case: &str) -> ExitCode {
	tracing::warn!(role, case, "unsupported test case");
	ExitCode::from(UNSUPPORTED)
}

/// The server's behavior for a runner test case, or `None` when unsupported.
fn server_case(case: &str) -> Option<Accept> {
	match case {
		// `chacha20`: the client offers only ChaCha20, which the full provider negotiates.
		// `keyupdate` reaches the server as `transfer`, the loss and corruption cases as
		// `transfer` or `multiconnect`, and `ecn` needs only the socket's ECN marking.
		"handshake" | "transfer" | "chacha20" | "resumption" | "multiconnect" | "ecn" => Some(Accept::Direct),
		"retry" => Some(Accept::Retry),
		// `connectionmigration` needs a preferred address served by the same endpoint,
		// `zerortt` early data, `v2` QUIC v2, and `http3` an HTTP/3 server.
		_ => None,
	}
}

/// The client's behavior for a runner test case, or `None` when unsupported.
fn client_case(case: &str) -> Option<Fetch> {
	match case {
		// moq-quic updates its keys on its own within the first thousand packets, which
		// `keyupdate`'s 3 MB transfer always exceeds. `retry` needs nothing of the client:
		// moq-quic answers a Retry by itself.
		"handshake" | "transfer" | "keyupdate" | "retry" | "ecn" => Some(Fetch::One),
		"resumption" => Some(Fetch::Resume),
		"multiconnect" => Some(Fetch::Each),
		// `chacha20` needs a client offering only ChaCha20, but moq-quic keys Initial
		// packets from the TLS config's own AES-128-GCM suite, so such a config cannot dial.
		_ => None,
	}
}

async fn serve(accept: Accept) -> anyhow::Result<()> {
	let chain = CertificateDer::pem_file_iter("/certs/cert.pem")
		.context("open /certs/cert.pem")?
		.collect::<Result<Vec<_>, _>>()
		.context("parse /certs/cert.pem")?;
	let key = PrivateKeyDer::from_pem_file("/certs/priv.key").context("parse /certs/priv.key")?;
	let mut tls = rustls::ServerConfig::builder_with_provider(moq_tokio::crypto::provider()?)
		.with_protocol_versions(&[&rustls::version::TLS13])?
		.with_no_client_auth()
		.with_single_cert(chain, key)?;
	tls.alpn_protocols = vec![ALPN.to_vec()];
	tls.key_log = Arc::new(rustls::KeyLogFile::new());

	let config = endpoint::Config::default().with_server(server::Config::new(Arc::new(tls)));
	// Dual-stack, so the `ipv6` case reaches the same endpoint.
	let socket = UdpSocket::bind((Ipv6Addr::UNSPECIFIED, 443)).context("bind [::]:443")?;
	let endpoint = Endpoint::new(config, socket)?;
	tracing::info!(addr = %endpoint.local_addr(), ?accept, "serving /www");

	while let Some(incoming) = endpoint.accept().await {
		tokio::spawn(async move {
			if let Err(err) = connection(incoming, accept).await {
				tracing::warn!("connection: {err:#}");
			}
		});
	}
	Ok(())
}

async fn connection(incoming: Incoming, accept: Accept) -> anyhow::Result<()> {
	let incoming = match accept {
		Accept::Direct => incoming,
		Accept::Retry => match incoming.validate_address() {
			Some(incoming) => incoming,
			None => return Ok(()),
		},
	};
	let conn = incoming.accept()?.await?;
	tracing::info!(remote = %conn.remote_address(), "accepted");
	loop {
		let (send, recv) = match conn.accept_bi().await {
			Ok(stream) => stream,
			// The client closing is how every case ends.
			Err(err) => {
				tracing::info!("closed: {err}");
				return Ok(());
			}
		};
		tokio::spawn(async move {
			if let Err(err) = respond(send, recv).await {
				tracing::warn!("request: {err:#}");
			}
		});
	}
}

/// Answer one `GET /path` with the file under `/www`.
async fn respond(mut send: send::Stream, mut recv: recv::Stream) -> anyhow::Result<()> {
	let request = recv.read_to_end(MAX_REQUEST).await?;
	let request = std::str::from_utf8(&request).context("request is not UTF-8")?;
	let path = request
		.trim_end()
		.strip_prefix("GET ")
		.with_context(|| format!("not a GET: {request:?}"))?;
	let path = www_path(path)?;
	let mut file = tokio::fs::File::open(&path)
		.await
		.with_context(|| format!("open {}", path.display()))?;
	let mut buf = vec![0; CHUNK];
	loop {
		let n = file.read(&mut buf).await?;
		if n == 0 {
			break;
		}
		send.write_all(&buf[..n]).await?;
	}
	send.finish();
	Ok(())
}

/// Map a request path into `/www`, refusing one that climbs out of it.
fn www_path(path: &str) -> anyhow::Result<PathBuf> {
	let relative = Path::new(path.trim_start_matches('/'));
	ensure!(
		relative.components().all(|c| matches!(c, Component::Normal(_))),
		"path {path:?} leaves /www"
	);
	Ok(Path::new("/www").join(relative))
}

async fn fetch_all(fetch: Fetch) -> anyhow::Result<()> {
	let requests = std::env::var("REQUESTS").context("REQUESTS is unset")?;
	let urls = requests
		.split_whitespace()
		.map(url::Url::parse)
		.collect::<Result<Vec<_>, _>>()
		.context("parse REQUESTS")?;
	let Some(first) = urls.first() else {
		bail!("REQUESTS is empty");
	};
	// Every request names the same server.
	let host = first.host_str().context("request without a host")?.to_string();
	let port = first.port_or_known_default().context("request without a port")?;
	let remote = tokio::net::lookup_host((host.as_str(), port))
		.await?
		.next()
		.with_context(|| format!("{host} does not resolve"))?;

	// The runner tests QUIC, not the PKI: `amplificationlimit` serves a chain deeper than
	// webpki verifies. The handshake signature is still checked.
	let mut connect = moq_tokio::tls::Connect::default();
	connect.insecure = Some(true);
	let mut tls = connect.build()?;
	tls.alpn_protocols = vec![ALPN.to_vec()];
	tls.key_log = Arc::new(rustls::KeyLogFile::new());
	// One config for every connection, so the second `resumption` dial finds the first's ticket.
	let config = client::Config::new(Arc::new(tls));

	let local: SocketAddr = match remote {
		SocketAddr::V4(_) => (std::net::Ipv4Addr::UNSPECIFIED, 0).into(),
		SocketAddr::V6(_) => (Ipv6Addr::UNSPECIFIED, 0).into(),
	};
	let endpoint = Endpoint::new(endpoint::Config::default(), UdpSocket::bind(local)?)?;
	let dial = || async { anyhow::Ok(endpoint.connect(&config, remote, &host)?.await?) };

	let result = async {
		match fetch {
			Fetch::One => download(&dial().await?, &urls).await,
			Fetch::Resume => {
				let (first, rest) = urls.split_at(1);
				let conn = dial().await?;
				download(&conn, first).await?;
				conn.close(VarInt::from_u32(0), "");
				download(&dial().await?, rest).await
			}
			Fetch::Each => {
				for url in &urls {
					let conn = dial().await?;
					download(&conn, std::slice::from_ref(url)).await?;
					conn.close(VarInt::from_u32(0), "");
				}
				Ok(())
			}
		}
	}
	.await;
	// Every connection closes with NO_ERROR, which `transfer` checks, and the close goes out
	// before the process exits.
	endpoint.close(VarInt::from_u32(0), "").await;
	result
}

/// Fetch each URL on its own stream of `conn`, in parallel, into `/downloads`.
async fn download(conn: &Connection, urls: &[url::Url]) -> anyhow::Result<()> {
	let fetches = urls.iter().map(|url| async move {
		let (mut send, mut recv) = conn.open_bi(Default::default()).await?;
		send.write_all(format!("GET {}\r\n", url.path()).as_bytes()).await?;
		send.finish();
		let target = Path::new("/downloads").join(url.path().trim_start_matches('/'));
		let mut file = tokio::fs::File::create(&target)
			.await
			.with_context(|| format!("create {}", target.display()))?;
		while let Some(chunk) = recv.read_chunk(CHUNK).await? {
			tokio::io::AsyncWriteExt::write_all(&mut file, &chunk).await?;
		}
		anyhow::Ok(())
	});
	futures::future::try_join_all(fetches).await?;
	Ok(())
}
