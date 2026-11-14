//! `moq tcp`: forward TCP connections through a relay as socket sessions, so an
//! unmodified client reaches an unmodified server.
//!
//! `tcp accept <id>` is a socket server: each session dialed to it connects to a local
//! TCP address. `tcp dial <peer>` listens on a local TCP address and dials `peer` for
//! each connection it accepts. See `doc/concept/socket.md`.

use std::net::SocketAddr;

use crate::args::MoqSide;
use anyhow::Context;
use moq_net::socket::Endpoint;
use moq_tokio::socket::TcpStream;

/// Forward TCP connections through a relay.
#[derive(usage_rs::Args, Clone)]
#[usage(unknown_flags = "error", args_override_self = false)]
pub struct Args {
	/// Which end of the forward this process is.
	#[usage(subcommand)]
	pub side: Side,
}

/// Which end of a forward this process is.
#[derive(usage_rs::Subcommands, Clone)]
pub enum Side {
	/// Listen on `--local` and dial the socket server `<peer>` for each connection.
	Dial(Dial),
	/// Be the socket server `<id>`, connecting each session dialed to it to `--local`.
	Accept(Accept),
}

/// `tcp dial`: the client end.
#[derive(usage_rs::Args, Clone)]
#[usage(unknown_flags = "error", args_override_self = false)]
pub struct Dial {
	/// The socket server to dial, as `tcp accept` names it.
	pub peer: String,

	/// The local TCP address to accept connections on, e.g. `127.0.0.1:2222`.
	#[usage(long)]
	pub local: SocketAddr,

	#[usage(flatten)]
	pub socket: Socket,
}

/// `tcp accept`: the server end.
#[derive(usage_rs::Args, Clone)]
#[usage(unknown_flags = "error", args_override_self = false)]
pub struct Accept {
	/// This socket server's id, which `tcp dial` names. One path segment.
	pub id: String,

	/// The local TCP address each session connects to, e.g. `127.0.0.1:22`.
	#[usage(long)]
	pub local: SocketAddr,

	#[usage(flatten)]
	pub socket: Socket,
}

/// What both ends agree on, and how each reads.
#[derive(usage_rs::Args, Clone)]
pub struct Socket {
	/// The path under the connection where the socket's endpoints meet; the connection
	/// path itself by default.
	#[usage(long)]
	pub prefix: Option<String>,

	/// How the bytes this end receives are scheduled against the relay's other traffic,
	/// as a subscription's priority: -128 to 127, higher first.
	#[usage(long, default = "0")]
	pub priority: i8,
}

/// The versions offered when `--connect-version` names none: every one whose sessions
/// carry a socket's hidden broadcasts.
const VERSIONS: [&str; 4] = [
	"moq-lite-07-wip",
	"moq-transport-22",
	"moq-transport-20",
	"moq-transport-18",
];

/// Forward TCP connections until the session to the relay ends.
pub async fn run(moq: MoqSide, args: Args, net: crate::Net) -> anyhow::Result<()> {
	let url = moq
		.client
		.url
		.clone()
		.context("`tcp` dials a relay: pass --connect <url>")?;
	let mut config = moq.client.clone();
	if config.version.is_empty() {
		config.version = VERSIONS
			.iter()
			.map(|version| version.parse().map_err(anyhow::Error::msg))
			.collect::<anyhow::Result<_>>()?;
	}

	let origin = moq_tokio::origin::spawn();
	let client = net
		.client(config)?
		.with_publisher(origin.consume())
		.with_subscriber(origin.clone());
	let connection = client.connect(url).established().await.context("failed to connect")?;

	let serve = async {
		match args.side {
			Side::Dial(dial) => run_dial(&origin, dial).await,
			Side::Accept(accept) => run_accept(&origin, accept).await,
		}
	};
	// The connection redials on its own; once it gives up, nothing can reach the peer.
	let result = tokio::select! {
		result = serve => result,
		closed = connection.closed() => match closed {
			Ok(()) => Err(anyhow::anyhow!("the connection to the relay closed")),
			Err(err) => Err(err).context("the connection to the relay failed"),
		},
	};
	let _ = connection.close().await;
	client.close().await;
	result
}

async fn run_dial(origin: &moq_net::origin::Producer, args: Dial) -> anyhow::Result<()> {
	let id = format!("{:032x}", rand::random::<u128>());
	let endpoint =
		Endpoint::new(origin, args.socket.prefix.as_deref().unwrap_or_default(), &id).context("invalid --prefix")?;
	let listener = tokio::net::TcpListener::bind(args.local)
		.await
		.with_context(|| format!("failed to listen on {}", args.local))?;
	tracing::info!(local = %args.local, peer = %args.peer, "forwarding");

	loop {
		let (local, addr) = listener.accept().await.context("failed to accept a local connection")?;
		let dial = endpoint
			.dial_tcp(&args.peer, args.socket.priority)
			.context("failed to dial")?;
		tokio::spawn(async move {
			let stream = match dial.await {
				Ok(stream) => TcpStream::from(stream),
				Err(err) => return tracing::warn!(%addr, %err, "dial failed"),
			};
			forward(local, stream, addr).await;
		});
	}
}

async fn run_accept(origin: &moq_net::origin::Producer, args: Accept) -> anyhow::Result<()> {
	let endpoint = Endpoint::new(origin, args.socket.prefix.as_deref().unwrap_or_default(), &args.id)
		.context("invalid --prefix or id")?;
	tracing::info!(id = %args.id, local = %args.local, "accepting");

	loop {
		let incoming = endpoint.accept_tcp().await.context("stopped accepting")?;
		let (target, priority) = (args.local, args.socket.priority);
		tokio::spawn(async move {
			// Connected before the dial is accepted, so a refused connection refuses the dial.
			let local = match tokio::net::TcpStream::connect(target).await {
				Ok(local) => local,
				Err(err) => {
					tracing::warn!(%target, %err, "failed to connect");
					return incoming.reject();
				}
			};
			let stream = match incoming.accept(priority).await {
				Ok(stream) => TcpStream::from(stream),
				Err(err) => return tracing::warn!(%err, "accept failed"),
			};
			forward(local, stream, target).await;
		});
	}
}

/// Copy both ways until both halves end, passing each FIN on as a shutdown. A failure
/// either way resets both: dropping the session resets it, and the local socket closes
/// with a reset.
async fn forward(mut local: tokio::net::TcpStream, mut stream: TcpStream, addr: SocketAddr) {
	let _ = local.set_nodelay(true);
	match tokio::io::copy_bidirectional_with_sizes(
		&mut local,
		&mut stream,
		moq_tokio::socket::MAX_FRAME,
		moq_tokio::socket::MAX_FRAME,
	)
	.await
	{
		Ok((up, down)) => tracing::debug!(%addr, up, down, "closed"),
		Err(err) => {
			tracing::debug!(%addr, %err, "reset");
			// Closed with a reset rather than a FIN, as the peer's failure was.
			let _ = local.set_zero_linger();
		}
	}
}
