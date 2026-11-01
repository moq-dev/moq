//! Echo load over loopback, for profiling where qmux spends CPU on each stream transport.
//!
//! Every case connects `sessions` client/server pairs over loopback TCP. Each client
//! keeps one message of `size` bytes in flight on one bidirectional stream, and its
//! server echoes it back, so messages are never coalesced. A message that fits in one
//! record (16382 bytes by default, less the frame header) crosses as exactly one; a
//! larger one is split, so the per-message numbers then cover several records. Both
//! ends run in this process on one current-thread runtime, so the CPU covers sending
//! and receiving alike.
//!
//! It reports process CPU time (user plus system), not wall clock, so it still means
//! something under a profiler; `bench/README.md` shows how to run and profile it.
//!
//! ```bash
//! cargo run --release -p qmux@0.7.0 --example bench -- --transport ws,wss --sessions 1,64 --size 300
//! ```
//!
//! A local profiling tool, not a regression series: the moq-net session bench owns
//! qmux's series in the nightly table.

use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use anyhow::{Context as _, bail};
use bytes::Bytes;
use moq_net::transport::poll::{RecvStream as _, SendOptions, SendStream as _, Session as _};
use qmux::{Session, Version};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinSet;

const USAGE: &str =
	"usage: bench [--transport tcp,tls,ws,wss] [--sessions 1,16,256] [--size 64,1024,16000] [--seconds 2]";

#[derive(Clone, Copy, Debug)]
enum Transport {
	Tcp,
	Tls,
	Ws,
	Wss,
}

impl Transport {
	fn parse(value: &str) -> anyhow::Result<Self> {
		Ok(match value {
			"tcp" => Self::Tcp,
			"tls" => Self::Tls,
			"ws" => Self::Ws,
			"wss" => Self::Wss,
			_ => bail!("unknown transport {value:?}; expected tcp, tls, ws, or wss"),
		})
	}

	fn name(self) -> &'static str {
		match self {
			Self::Tcp => "tcp",
			Self::Tls => "tls",
			Self::Ws => "ws",
			Self::Wss => "wss",
		}
	}
}

struct Args {
	transports: Vec<Transport>,
	sessions: Vec<usize>,
	sizes: Vec<usize>,
	duration: Duration,
}

impl Args {
	fn parse() -> anyhow::Result<Self> {
		let mut args = Self {
			transports: vec![Transport::Tcp, Transport::Tls, Transport::Ws, Transport::Wss],
			sessions: vec![1, 16, 256],
			sizes: vec![64, 1024, 16000],
			duration: Duration::from_secs(2),
		};
		let mut argv = std::env::args().skip(1);
		while let Some(flag) = argv.next() {
			let value = argv.next().with_context(|| format!("{flag} needs a value\n{USAGE}"))?;
			match flag.as_str() {
				"--transport" => args.transports = list(&value, Transport::parse)?,
				"--sessions" => args.sessions = list(&value, |v| Ok(v.parse::<usize>()?))?,
				"--size" => args.sizes = list(&value, |v| Ok(v.parse::<usize>()?))?,
				"--seconds" => args.duration = Duration::try_from_secs_f64(value.parse()?)?,
				_ => bail!("unknown flag {flag}\n{USAGE}"),
			}
		}
		if args.sessions.contains(&0) || args.sizes.contains(&0) || args.duration.is_zero() {
			bail!("sessions, sizes, and the duration must be positive");
		}
		Ok(args)
	}
}

fn list<T>(value: &str, parse: impl Fn(&str) -> anyhow::Result<T>) -> anyhow::Result<Vec<T>> {
	value
		.split(',')
		.map(|item| parse(item.trim()).with_context(|| format!("invalid value {item:?}")))
		.collect()
}

/// User plus system CPU time of every thread in this process.
#[cfg(unix)]
fn process_cpu() -> anyhow::Result<Duration> {
	let mut usage = std::mem::MaybeUninit::<libc::rusage>::uninit();
	// SAFETY: `getrusage` fills the struct it is handed and reports failure by return value.
	if unsafe { libc::getrusage(libc::RUSAGE_SELF, usage.as_mut_ptr()) } != 0 {
		return Err(std::io::Error::last_os_error()).context("getrusage");
	}
	// SAFETY: a zero return means the kernel initialized the struct.
	let usage = unsafe { usage.assume_init() };
	let time = |tv: libc::timeval| Duration::new(tv.tv_sec as u64, tv.tv_usec as u32 * 1000);
	Ok(time(usage.ru_utime) + time(usage.ru_stime))
}

#[cfg(not(unix))]
fn process_cpu() -> anyhow::Result<Duration> {
	bail!("process CPU time needs getrusage, so this bench runs on Unix only")
}

/// A self-signed `localhost` identity both TLS transports share.
struct Tls {
	server: Arc<rustls::ServerConfig>,
	client: Arc<rustls::ClientConfig>,
}

impl Tls {
	fn new() -> anyhow::Result<Self> {
		let identity = rcgen::generate_simple_self_signed(vec!["localhost".into()])?;
		let cert = CertificateDer::from(identity.cert.der().to_vec());
		let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(identity.signing_key.serialize_der()));
		// The relay's provider, so the TLS share matches what it pays.
		let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());

		let mut server = rustls::ServerConfig::builder_with_provider(provider.clone())
			.with_safe_default_protocol_versions()?
			.with_no_client_auth()
			.with_single_cert(vec![cert.clone()], key)?;
		server.alpn_protocols = vec![Version::QMux02.alpn().as_bytes().to_vec()];

		let mut roots = rustls::RootCertStore::empty();
		roots.add(cert)?;
		let client = rustls::ClientConfig::builder_with_provider(provider)
			.with_safe_default_protocol_versions()?
			.with_root_certificates(roots)
			.with_no_client_auth();

		Ok(Self {
			server: Arc::new(server),
			client: Arc::new(client),
		})
	}
}

async fn accept(transport: Transport, tls: &Tls, socket: TcpStream) -> anyhow::Result<Session> {
	Ok(match transport {
		Transport::Tcp => qmux::tcp::accept(socket, qmux::Config::default()).await?,
		Transport::Tls => qmux::tls::Server::new(tls.server.clone()).accept(socket).await?,
		Transport::Ws => {
			socket.set_nodelay(true)?;
			qmux::ws::Server::new().accept(socket).await?
		}
		Transport::Wss => {
			socket.set_nodelay(true)?;
			let socket = tokio_rustls::TlsAcceptor::from(tls.server.clone())
				.accept(socket)
				.await?;
			qmux::ws::Server::new().accept(socket).await?
		}
	})
}

async fn connect(transport: Transport, tls: &Tls, addr: SocketAddr) -> anyhow::Result<Session> {
	Ok(match transport {
		Transport::Tcp => qmux::tcp::connect(addr, qmux::Config::default()).await?,
		Transport::Tls => {
			qmux::tls::Client::new(tls.client.clone())
				.connect(addr, "localhost")
				.await?
		}
		Transport::Ws => qmux::ws::Client::new().connect(&format!("ws://{addr}")).await?,
		Transport::Wss => {
			let connector = qmux::ws::tokio_tungstenite::Connector::Rustls(tls.client.clone());
			qmux::ws::Client::new()
				.with_connector(connector)
				.connect(&format!("wss://localhost:{}", addr.port()))
				.await?
		}
	})
}

/// Echo every chunk back on the same stream until the client finishes it.
async fn serve(session: Session) -> anyhow::Result<()> {
	let (mut send, mut recv) = session.accept_bi().await?;
	while let Some(chunk) = recv.read_chunk(usize::MAX).await? {
		send.write_chunk(chunk).await?;
	}
	Ok(())
}

/// Send `payload`, read it back whole, and count the round trip, until aborted.
async fn drive(session: Session, payload: Bytes, done: Arc<AtomicU64>) -> anyhow::Result<()> {
	let (mut send, mut recv) = session.open_bi(SendOptions::default()).await?;
	let mut echoed = Vec::with_capacity(payload.len());
	loop {
		send.write_chunk(payload.clone()).await?;
		echoed.clear();
		while echoed.len() < payload.len() {
			let chunk = recv
				.read_chunk(payload.len() - echoed.len())
				.await?
				.context("echo ended early")?;
			echoed.extend_from_slice(&chunk);
		}
		// Checked whole, so skipped work cannot read as a speedup.
		if echoed != payload {
			bail!("echo did not match");
		}
		done.fetch_add(1, Ordering::Relaxed);
	}
}

struct Sample {
	round_trips: u64,
	wall: Duration,
	cpu: Duration,
}

async fn measure(
	transport: Transport,
	tls: &Tls,
	sessions: usize,
	size: usize,
	duration: Duration,
) -> anyhow::Result<Sample> {
	let listener = TcpListener::bind("127.0.0.1:0").await?;
	let addr = listener.local_addr()?;
	// Patterned so a misordered or mixed-up echo fails the comparison.
	let payload = Bytes::from((0..size).map(|index| (index % 251) as u8).collect::<Vec<_>>());
	let done = Arc::new(AtomicU64::new(0));

	// Handshakes stay outside the measurement. Each side awaits the other's, so they run together.
	let mut pairs = Vec::with_capacity(sessions);
	for _ in 0..sessions {
		let server = async { accept(transport, tls, listener.accept().await?.0).await };
		let (client, server) = tokio::join!(connect(transport, tls, addr), server);
		pairs.push((client?, server?));
	}

	let mut tasks = JoinSet::new();
	for (client, server) in pairs {
		tasks.spawn(serve(server));
		tasks.spawn(drive(client, payload.clone(), done.clone()));
	}

	// Let every stream open and the allocator settle before the window starts.
	kio::clock::sleep(duration / 4).await;
	let (start, cpu, count) = (kio::clock::now(), process_cpu()?, done.load(Ordering::Relaxed));
	kio::clock::sleep(duration).await;
	let sample = Sample {
		round_trips: done.load(Ordering::Relaxed) - count,
		cpu: process_cpu()? - cpu,
		wall: start.elapsed(),
	};

	// Both loops run until aborted, so any that already returned failed.
	if let Some(result) = tasks.try_join_next() {
		result??;
		bail!("a session ended during the measurement");
	}
	tasks.shutdown().await;
	if sample.round_trips == 0 {
		bail!(
			"{} with {sessions} sessions of {size} B echoed nothing",
			transport.name()
		);
	}
	Ok(sample)
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> {
	let args = Args::parse()?;
	let tls = Tls::new()?;

	println!(
		"{:<9} {:>8} {:>7} {:>11} {:>9} {:>10} {:>11} {:>12}",
		"transport", "sessions", "size-B", "msgs/s", "Gbps", "CPU-cores", "CPU-us/msg", "cores/Gbps"
	);
	for &transport in &args.transports {
		for &sessions in &args.sessions {
			for &size in &args.sizes {
				let sample = measure(transport, &tls, sessions, size, args.duration)
					.await
					.with_context(|| format!("{} with {sessions} sessions of {size} B", transport.name()))?;
				// Every round trip is two messages of `size` payload bytes, one each way.
				let messages = 2.0 * sample.round_trips as f64;
				let wall = sample.wall.as_secs_f64();
				let cpu = sample.cpu.as_secs_f64();
				let gbps = messages * size as f64 * 8.0 / wall / 1e9;
				println!(
					"{:<9} {:>8} {:>7} {:>11.0} {:>9.3} {:>10.3} {:>11.3} {:>12.3}",
					transport.name(),
					sessions,
					size,
					messages / wall,
					gbps,
					cpu / wall,
					cpu * 1e6 / messages,
					cpu / wall / gbps,
				);
			}
		}
	}
	Ok(())
}
