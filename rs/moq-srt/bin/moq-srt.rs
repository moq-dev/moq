//! `moq-srt` binary.
//!
//! Runs an SRT gateway (MPEG-TS in via `m=publish`, out via `m=request`) and
//! connects it to MoQ two ways:
//!
//! - `serve` runs a local QUIC/WebTransport server so subscribers connect
//!   straight to this binary (no separate relay needed). Ingested broadcasts are
//!   also requestable back out over SRT.
//! - `publish` forwards every ingested broadcast out to a remote relay over
//!   WebTransport, like `moq-hls import` / `moq-rtc` WHIP. SRT requests are
//!   served from the local origin (broadcasts ingested by this same process).
//!
//! A relay that wants an in-process gateway should instead depend on the
//! `moq-srt` library and call `moq_srt::run` against its own origin.

mod serve;
mod web;

use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::Context;
use clap::{Args, Parser, Subcommand};
use url::Url;

#[derive(Parser, Clone)]
#[command(name = "moq-srt", version)]
struct Cli {
	#[command(flatten)]
	log: moq_native::Log,

	#[command(subcommand)]
	command: Command,
}

#[derive(Subcommand, Clone)]
enum Command {
	/// Ingest SRT and serve it directly as a local relay.
	Serve {
		/// The QUIC/WebTransport server configuration.
		#[command(flatten)]
		config: moq_native::ServerConfig,

		/// Optionally serve static files (e.g. a web player) from this directory.
		#[arg(long)]
		dir: Option<PathBuf>,

		#[command(flatten)]
		srt: SrtArgs,
	},
	/// Ingest SRT and publish the broadcasts to a remote MoQ relay.
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
		#[arg(long, env = "MOQ_SRT_RELAY")]
		relay: Url,

		#[command(flatten)]
		srt: SrtArgs,
	},
}

/// CLI flags for the SRT listener, converted into a [`moq_srt::Config`].
#[derive(Args, Clone)]
struct SrtArgs {
	/// Address to listen on for SRT ingest (e.g. `0.0.0.0:9000`).
	#[arg(long = "srt-listen", env = "MOQ_SRT_LISTEN")]
	listen: SocketAddr,

	/// Prefix prepended to every ingested broadcast path (e.g. `live/`).
	#[arg(long = "srt-prefix", env = "MOQ_SRT_PREFIX", default_value = "")]
	prefix: String,

	/// SRT receive latency: the negotiated buffer that trades delay for loss recovery.
	#[arg(long = "srt-latency", env = "MOQ_SRT_LATENCY", default_value = "200ms", value_parser = humantime::parse_duration)]
	latency: Duration,
}

impl From<SrtArgs> for moq_srt::Config {
	fn from(args: SrtArgs) -> Self {
		let mut config = moq_srt::Config::default();
		config.listen = Some(args.listen);
		config.prefix = args.prefix;
		config.latency = args.latency;
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
		Command::Serve { config, dir, srt } => run_serve(config, dir, srt.into()).await,
		Command::Publish { config, relay, srt } => run_publish(config, relay, srt.into()).await,
	}
}

/// Run a local QUIC/WebTransport server and ingest SRT directly into it.
async fn run_serve(config: moq_native::ServerConfig, dir: Option<PathBuf>, srt: moq_srt::Config) -> anyhow::Result<()> {
	let web_bind = config.bind.clone().unwrap_or_else(|| "[::]:443".to_string());

	let server = config.init().context("init moq server")?;
	let web_tls = server.tls_info();

	// SRT publishes broadcasts into this origin; the server serves them out.
	let origin = moq_net::Origin::random().produce();

	tracing::info!(%web_bind, "moq-srt serving");

	#[cfg(unix)]
	let _ = sd_notify::notify(&[sd_notify::NotifyState::Ready]);

	tokio::select! {
		res = serve::run(server, origin.clone()) => res.context("moq server failed"),
		res = web::run(&web_bind, web_tls, dir) => res.context("web server failed"),
		res = moq_srt::run(origin, srt) => res.context("srt ingest failed"),
		_ = tokio::signal::ctrl_c() => Ok(()),
	}
}

/// Ingest SRT and forward every broadcast to a remote relay at `relay`.
async fn run_publish(config: moq_native::ClientConfig, relay: Url, srt: moq_srt::Config) -> anyhow::Result<()> {
	let client = config.init().context("init moq client")?;

	// SRT publishes broadcasts into this origin; the client forwards them on.
	let origin = moq_net::Origin::random().produce();
	let reconnect = client.with_publisher(&origin).reconnect(relay.clone());

	tracing::info!(%relay, "moq-srt publishing");

	#[cfg(unix)]
	let _ = sd_notify::notify(&[sd_notify::NotifyState::Ready]);

	tokio::select! {
		res = moq_srt::run(origin, srt) => res.context("srt ingest failed"),
		res = reconnect.closed() => res.context("relay connection failed"),
		_ = tokio::signal::ctrl_c() => Ok(()),
	}
}
