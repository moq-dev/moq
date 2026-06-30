//! `moq-rtmp` binary.
//!
//! Bridges RTMP / enhanced-RTMP (OBS, ffmpeg, hardware encoders, and players like
//! VLC / ffplay / mpv) to MoQ. Each listener does both directions: a `publish`
//! ingests a stream in, a `play` pulls one back out (`rtmp://host/<app>/<key>`
//! round-trips to the same path). Two binary modes wire up the MoQ side:
//!
//! - `serve` runs a local QUIC/WebTransport server so MoQ subscribers connect
//!   straight to this binary (no separate relay needed). RTMP players can also
//!   pull the same broadcasts back out.
//! - `publish` forwards every ingested broadcast out to a remote relay over
//!   WebTransport, like `moq-srt publish` / `moq-hls import` / `moq-rtc` WHIP.
//!
//! A relay that wants in-process ingest/egress should instead depend on the
//! `moq-rtmp` library and call `moq_rtmp::run` against its own origin.

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

/// CLI flags for the RTMP listener(s), converted into [`moq_rtmp::Config`]s.
#[derive(Args, Clone)]
struct RtmpArgs {
	/// Address to listen on for plaintext RTMP ingest (`rtmp://`). RTMP's
	/// well-known port is 1935.
	#[arg(long = "rtmp-listen", env = "MOQ_RTMP_LISTEN", default_value = "[::]:1935")]
	listen: SocketAddr,

	/// Prefix prepended to every ingested broadcast path (e.g. `live/`).
	#[arg(long = "rtmp-prefix", env = "MOQ_RTMP_PREFIX", default_value = "")]
	prefix: String,

	/// Also listen for RTMPS (RTMP over TLS, `rtmps://`) on this address, in
	/// addition to plaintext RTMP. Requires a certificate via `--rtmps-tls-cert`
	/// /`--rtmps-tls-key` or a self-signed `--rtmps-tls-generate`. RTMPS has no
	/// well-known port; common choices are 443 or a custom one.
	///
	/// The bundled RTMPS terminator reuses moq-native's certificate loader, which
	/// is only built with the `noq` or `quinn` backend; these flags are absent in
	/// other builds.
	#[cfg(any(feature = "noq", feature = "quinn"))]
	#[arg(long = "rtmps-listen", env = "MOQ_RTMPS_LISTEN")]
	rtmps_listen: Option<SocketAddr>,

	/// PEM certificate chain for RTMPS.
	#[cfg(any(feature = "noq", feature = "quinn"))]
	#[arg(long = "rtmps-tls-cert", env = "MOQ_RTMPS_TLS_CERT")]
	rtmps_cert: Option<PathBuf>,

	/// PEM private key for RTMPS.
	#[cfg(any(feature = "noq", feature = "quinn"))]
	#[arg(long = "rtmps-tls-key", env = "MOQ_RTMPS_TLS_KEY")]
	rtmps_key: Option<PathBuf>,

	/// Or generate a self-signed RTMPS certificate for these hostnames (testing
	/// only; clients must disable verification or pin the fingerprint).
	#[cfg(any(feature = "noq", feature = "quinn"))]
	#[arg(long = "rtmps-tls-generate", env = "MOQ_RTMPS_TLS_GENERATE", value_delimiter = ',')]
	rtmps_generate: Vec<String>,
}

impl RtmpArgs {
	/// Build the listener configs: always plaintext RTMP, plus RTMPS when
	/// `--rtmps-listen` is set. Both share the `--rtmp-prefix`.
	fn configs(&self) -> anyhow::Result<Vec<moq_rtmp::Config>> {
		let mut plain = moq_rtmp::Config::default();
		plain.listen = Some(self.listen);
		plain.prefix = self.prefix.clone();
		let mut configs = vec![plain];

		if let Some(addr) = self.rtmps_listen {
			// Reuse moq-native's cert loader (on-disk pair or self-signed). RTMPS
			// has no ALPN convention, so advertise none.
			let mut tls = moq_native::tls::Server::default();
			tls.cert = self.rtmps_cert.clone().into_iter().collect();
			tls.key = self.rtmps_key.clone().into_iter().collect();
			tls.generate = self.rtmps_generate.clone();
			let server_config = tls.server_config(vec![]).context("build RTMPS TLS config")?;

			let mut rtmps = moq_rtmp::Config::default();
			rtmps.listen = Some(addr);
			rtmps.prefix = self.prefix.clone();
			rtmps.tls = Some(server_config);
			configs.push(rtmps);
		}

		Ok(configs)
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
		Command::Serve { config, dir, rtmp } => run_serve(config, dir, rtmp.configs()?).await,
		Command::Publish { config, relay, rtmp } => run_publish(config, relay, rtmp.configs()?).await,
	}
}

/// Run every configured RTMP/RTMPS listener against `origin`, failing if any of
/// them does. Stays pending while at least one listener is alive.
async fn run_ingest(origin: moq_net::OriginProducer, configs: Vec<moq_rtmp::Config>) -> anyhow::Result<()> {
	use futures::StreamExt;

	let mut listeners: futures::stream::FuturesUnordered<_> = configs
		.into_iter()
		.map(|config| moq_rtmp::run(origin.clone(), config))
		.collect();

	while let Some(res) = listeners.next().await {
		res.context("rtmp ingest failed")?;
	}

	Ok(())
}

/// Run a local QUIC/WebTransport server and ingest RTMP directly into it.
async fn run_serve(
	config: moq_native::ServerConfig,
	dir: Option<PathBuf>,
	rtmp: Vec<moq_rtmp::Config>,
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
		res = run_ingest(origin, rtmp) => res,
		_ = tokio::signal::ctrl_c() => Ok(()),
	}
}

/// Ingest RTMP and forward every broadcast to a remote relay at `relay`.
async fn run_publish(config: moq_native::ClientConfig, relay: Url, rtmp: Vec<moq_rtmp::Config>) -> anyhow::Result<()> {
	let client = config.init().context("init moq client")?;

	// RTMP publishes broadcasts into this origin; the client forwards them on.
	let origin = moq_net::Origin::random().produce();
	let reconnect = client.with_publisher(&origin).reconnect(relay.clone());

	tracing::info!(%relay, "moq-rtmp publishing");

	#[cfg(unix)]
	let _ = sd_notify::notify(&[sd_notify::NotifyState::Ready]);

	tokio::select! {
		res = run_ingest(origin, rtmp) => res,
		res = reconnect.closed() => res.context("relay connection failed"),
		_ = tokio::signal::ctrl_c() => Ok(()),
	}
}
