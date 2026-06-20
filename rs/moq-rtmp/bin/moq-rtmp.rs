//! `moq-rtmp` binary.
//!
//! Ingests RTMP / enhanced-RTMP (OBS, ffmpeg, hardware encoders) and exposes it
//! over MoQ two ways:
//!
//! - `serve` runs a local QUIC/WebTransport server so subscribers connect
//!   straight to this binary (no separate relay needed).
//! - `publish` forwards every ingested broadcast out to a remote relay over
//!   WebTransport, like `moq-srt publish` / `moq-hls import` / `moq-rtc` WHIP.
//!
//! A relay that wants in-process ingest should instead depend on the `moq-rtmp`
//! library and call `moq_rtmp::run` against its own origin.

mod serve;
mod web;

use std::net::SocketAddr;
use std::path::PathBuf;

use anyhow::Context;
use clap::{Args, Parser, Subcommand};
use url::Url;

#[derive(Parser, Clone)]
#[command(name = "moq-rtmp", version)]
struct Cli {
	#[command(flatten)]
	log: moq_native::Log,

	#[command(subcommand)]
	command: Command,
}

#[derive(Subcommand, Clone)]
enum Command {
	/// Ingest RTMP and serve it directly as a local relay.
	Serve {
		/// The QUIC/WebTransport server configuration.
		#[command(flatten)]
		config: moq_native::ServerConfig,

		/// Optionally serve static files (e.g. a web player) from this directory.
		#[arg(long)]
		dir: Option<PathBuf>,

		#[command(flatten)]
		rtmp: RtmpArgs,
	},
	/// Ingest RTMP and publish the broadcasts to a remote MoQ relay.
	Publish {
		/// The MoQ client configuration.
		#[command(flatten)]
		config: moq_native::ClientConfig,

		/// URL of the MoQ relay to publish into (e.g. `https://relay.example.com`).
		///
		/// `https://` makes a WebTransport connection over QUIC. `http://` first
		/// fetches `/certificate.sha256` for the TLS fingerprint (insecure). The
		/// `?jwt=` query parameter supplies a moq-token-cli JWT; otherwise the
		/// public path (if any) is used.
		#[arg(long, env = "MOQ_RTMP_RELAY")]
		relay: Url,

		#[command(flatten)]
		rtmp: RtmpArgs,
	},
}

/// CLI flags for the RTMP listener, converted into a [`moq_rtmp::Config`].
#[derive(Args, Clone)]
struct RtmpArgs {
	/// Address to listen on for RTMP ingest. RTMP's well-known port is 1935.
	#[arg(long = "rtmp-listen", env = "MOQ_RTMP_LISTEN", default_value = "[::]:1935")]
	listen: SocketAddr,

	/// Prefix prepended to every ingested broadcast path (e.g. `live/`).
	#[arg(long = "rtmp-prefix", env = "MOQ_RTMP_PREFIX", default_value = "")]
	prefix: String,
}

impl From<RtmpArgs> for moq_rtmp::Config {
	fn from(args: RtmpArgs) -> Self {
		let mut config = moq_rtmp::Config::default();
		config.listen = Some(args.listen);
		config.prefix = args.prefix;
		config
	}
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
	// moq-native pulls in `ring` somewhere transitively, so install the
	// aws-lc-rs provider explicitly (mirrors moq-cli's main).
	rustls::crypto::aws_lc_rs::default_provider()
		.install_default()
		.expect("failed to install default crypto provider");

	let cli = Cli::parse();
	cli.log.init()?;

	match cli.command {
		Command::Serve { config, dir, rtmp } => run_serve(config, dir, rtmp.into()).await,
		Command::Publish { config, relay, rtmp } => run_publish(config, relay, rtmp.into()).await,
	}
}

/// Run a local QUIC/WebTransport server and ingest RTMP directly into it.
async fn run_serve(
	config: moq_native::ServerConfig,
	dir: Option<PathBuf>,
	rtmp: moq_rtmp::Config,
) -> anyhow::Result<()> {
	let web_bind = config.bind.clone().unwrap_or_else(|| "[::]:443".to_string());

	let server = config.init().context("init moq server")?;
	let web_tls = server.tls_info();

	// RTMP publishes broadcasts into this origin; the server serves them out.
	let origin = moq_net::Origin::random().produce();

	tracing::info!(%web_bind, "moq-rtmp serving");

	#[cfg(unix)]
	let _ = sd_notify::notify(&[sd_notify::NotifyState::Ready]);

	tokio::select! {
		res = serve::run(server, origin.clone()) => res.context("moq server failed"),
		res = web::run(&web_bind, web_tls, dir) => res.context("web server failed"),
		res = moq_rtmp::run(origin, rtmp) => res.context("rtmp ingest failed"),
		_ = tokio::signal::ctrl_c() => Ok(()),
	}
}

/// Ingest RTMP and forward every broadcast to a remote relay at `relay`.
async fn run_publish(config: moq_native::ClientConfig, relay: Url, rtmp: moq_rtmp::Config) -> anyhow::Result<()> {
	let client = config.init().context("init moq client")?;

	// RTMP publishes broadcasts into this origin; the client forwards them on.
	let origin = moq_net::Origin::random().produce();
	let reconnect = client.with_publisher(&origin).reconnect(relay.clone());

	tracing::info!(%relay, "moq-rtmp publishing");

	#[cfg(unix)]
	let _ = sd_notify::notify(&[sd_notify::NotifyState::Ready]);

	tokio::select! {
		res = moq_rtmp::run(origin, rtmp) => res.context("rtmp ingest failed"),
		res = reconnect.closed() => res.context("relay connection failed"),
		_ = tokio::signal::ctrl_c() => Ok(()),
	}
}
