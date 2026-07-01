//! The unified moq-cli argument surface.
//!
//! Grammar: `moq <import|export> <MoQ side> <protocol> <protocol opts>`.
//!
//! - `import` routes media INTO MoQ from one source; `export` routes it OUT to
//!   one sink. The verb fixes the data direction (and thus, for the
//!   bidirectional protocols, whether `--connect`/`--listen` push or pull).
//! - The MoQ side (`--client-connect` / `--server-bind`, both optional, at least
//!   one) attaches the shared Origin to the MoQ network. Both may be given: dial
//!   a relay *and* accept incoming sessions off the same Origin.
//! - Exactly one protocol subcommand per invocation, so "which endpoint" is
//!   unambiguous and there's no silently-ignored flag.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;

use clap::{ArgGroup, Args, Parser, Subcommand};
use url::Url;

use crate::publish::PublishFormat;
use crate::subscribe::{CatalogFormatArg, SubscribeFormat};

/// moq-cli: a media router that wires one endpoint onto a shared MoQ Origin.
#[derive(Parser, Clone)]
#[command(name = "moq", version = env!("VERSION"))]
pub struct Cli {
	#[command(flatten)]
	pub log: moq_native::Log,

	/// Iroh configuration.
	#[cfg(feature = "iroh")]
	#[command(flatten)]
	pub iroh: moq_native::iroh::EndpointConfig,

	#[command(subcommand)]
	pub direction: Direction,
}

/// The data direction, and the pivot between the MoQ side and the endpoint.
#[derive(Subcommand, Clone)]
pub enum Direction {
	/// Route media INTO MoQ from one source.
	Import(Import),
	/// Route media OUT OF MoQ to one sink.
	Export(Export),
}

/// The MoQ attachment, shared by both directions. At least one of
/// `--client-connect` / `--server-bind`; both may be given at once.
#[derive(Args, Clone)]
#[command(group = ArgGroup::new("moq").required(true).multiple(true).args(["client-connect", "server-bind"]))]
pub struct MoqSide {
	/// Dial a MoQ relay/server over WebTransport.
	///
	/// The URL path is the relay auth path (e.g. `/anon` for a public relay), not
	/// the broadcast; name the broadcast with `--broadcast`. `?jwt=` supplies a
	/// token. `http://` first fetches `/certificate.sha256` for the (insecure)
	/// self-signed fingerprint; `https://` connects directly.
	#[arg(
		id = "client-connect",
		long = "client-connect",
		env = "MOQ_CLIENT_CONNECT",
		help_heading = "MoQ"
	)]
	pub client_connect: Option<Url>,

	/// The broadcast name for a single-broadcast endpoint (stdin/stdout, HLS, and
	/// the foreign `--connect` dials). Ignored by the dynamic (`--listen`)
	/// endpoints, which name broadcasts from the protocol (RTMP app/key, SRT
	/// stream id).
	#[arg(long, alias = "name", help_heading = "MoQ")]
	pub broadcast: Option<String>,

	/// When hosting (`--server-bind`), also serve static files and the
	/// `/certificate.sha256` fingerprint over HTTP from this directory.
	#[arg(long, help_heading = "MoQ")]
	pub dir: Option<PathBuf>,

	/// MoQ client transport config (`--client-bind`, `--client-tls-*`, ...).
	#[command(flatten)]
	pub client: moq_native::ClientConfig,

	/// MoQ server transport config (`--server-bind`, `--server-tls-*`, `--tls-*`).
	#[command(flatten)]
	pub server: moq_native::ServerConfig,
}

impl MoqSide {
	/// The single-broadcast name (from `--broadcast`), if given.
	pub fn broadcast_name(&self) -> Option<String> {
		self.broadcast.clone()
	}

	/// The relay URL to dial, passed through unchanged (its path is the auth path).
	pub fn client_url(&self) -> Option<Url> {
		self.client_connect.clone()
	}
}

// ------------------------------------------------------------------ import

/// import = one source -> MoQ.
#[derive(Args, Clone)]
pub struct Import {
	#[command(flatten)]
	pub moq: MoqSide,

	#[command(subcommand)]
	pub source: ImportSource,
}

/// The single source feeding the Origin on an import.
#[derive(Subcommand, Clone)]
pub enum ImportSource {
	/// Read a container from stdin.
	Stdin {
		/// Container format on stdin.
		format: PublishFormat,
	},
	/// Pull a remote HLS / LL-HLS playlist (http/https URL or local file) into MoQ.
	Hls {
		/// Playlist URL or local file path.
		connect: String,
	},
	/// RTMP: pull a remote play (`--connect`) or accept incoming publishes (`--listen`).
	Rtmp(RtmpArgs),
	/// SRT: pull a remote stream (`--connect`) or accept incoming publishes (`--listen`).
	Srt(SrtArgs),
	/// WebRTC: WHEP client pulling a remote (`--connect`) or WHIP server accepting publishes (`--listen`).
	Rtc(RtcArgs),
}

// ------------------------------------------------------------------ export

/// export = MoQ -> one sink.
#[derive(Args, Clone)]
pub struct Export {
	#[command(flatten)]
	pub moq: MoqSide,

	#[command(subcommand)]
	pub sink: ExportSink,
}

/// The single sink draining the Origin on an export.
#[derive(Subcommand, Clone)]
pub enum ExportSink {
	/// Write a container to stdout.
	Stdout {
		/// Container format on stdout.
		format: SubscribeFormat,

		/// Maximum latency before skipping groups (e.g. `500ms`, `1s`).
		#[arg(long, default_value = "500ms", value_parser = humantime::parse_duration)]
		max_latency: Duration,

		/// Cap the output fragment duration (e.g. `2s`). Default: one GOP.
		#[arg(long, value_parser = humantime::parse_duration)]
		fragment_duration: Option<Duration>,

		/// Catalog format for track discovery (default: detect from the broadcast suffix).
		#[arg(long)]
		catalog: Option<CatalogFormatArg>,
	},
	/// Serve HLS / LL-HLS over HTTP.
	Hls {
		/// HTTP listener for the HLS endpoints.
		#[arg(long, default_value = "[::]:8089")]
		listen: SocketAddr,

		/// TLS certificates, keys, self-signed generation, and optional mTLS roots.
		#[command(flatten)]
		tls: moq_native::tls::Server,

		/// LL-HLS part target duration.
		#[arg(long, default_value = "500ms", value_parser = humantime::parse_duration)]
		part_target: Duration,

		/// Minimum media kept in each rendition's sliding window.
		#[arg(long, default_value = "16s", value_parser = humantime::parse_duration)]
		window: Duration,
	},
	/// RTMP: push to a remote (`--connect`) or serve plays (`--listen`).
	Rtmp(RtmpArgs),
	/// SRT: push to a remote (`--connect`) or serve requests (`--listen`).
	Srt(SrtArgs),
	/// WebRTC: WHIP client pushing to a remote (`--connect`) or WHEP server serving plays (`--listen`).
	Rtc(RtcArgs),
}

// ------------------------------------------------- shared foreign endpoints

/// RTMP endpoint: exactly one of `--connect` (dial) / `--listen` (bind). The
/// parent direction fixes whether that dial/bind pushes or pulls.
#[derive(Args, Clone)]
#[command(group = ArgGroup::new("rtmp-mode").required(true).multiple(false).args(["rtmp-connect", "rtmp-listen"]))]
pub struct RtmpArgs {
	/// Dial `rtmp://host[:1935]/<app>/<key>`.
	#[arg(id = "rtmp-connect", long = "connect", value_name = "URL")]
	pub connect: Option<Url>,

	/// Bind an RTMP listener. Broadcasts are named from the RTMP app/key.
	#[arg(id = "rtmp-listen", long = "listen", value_name = "ADDR")]
	pub listen: Option<SocketAddr>,

	/// Prefix prepended to the app/key when naming broadcasts.
	#[arg(long, requires = "rtmp-listen")]
	pub prefix: Option<String>,
}

/// SRT endpoint: exactly one of `--connect` / `--listen`.
#[derive(Args, Clone)]
#[command(group = ArgGroup::new("srt-mode").required(true).multiple(false).args(["srt-connect", "srt-listen"]))]
pub struct SrtArgs {
	/// Dial `srt://host:port?streamid=...`.
	#[arg(id = "srt-connect", long = "connect", value_name = "URL")]
	pub connect: Option<Url>,

	/// Bind an SRT listener. Broadcasts are named from the stream id.
	#[arg(id = "srt-listen", long = "listen", value_name = "ADDR")]
	pub listen: Option<SocketAddr>,

	/// Prefix prepended to the stream id when naming broadcasts.
	#[arg(long)]
	pub prefix: Option<String>,

	/// SRT receive latency: the negotiated buffer trading delay for loss recovery.
	#[arg(long, default_value = "200ms", value_parser = humantime::parse_duration)]
	pub latency: Duration,
}

/// WebRTC endpoint: exactly one of `--connect` (WHIP/WHEP client) / `--listen`
/// (WHIP/WHEP server). Which of WHIP or WHEP is chosen by the parent direction.
#[derive(Args, Clone)]
#[command(group = ArgGroup::new("rtc-mode").required(true).multiple(false).args(["rtc-connect", "rtc-listen"]))]
pub struct RtcArgs {
	/// Dial a remote WHIP/WHEP endpoint URL.
	#[arg(id = "rtc-connect", long = "connect", value_name = "URL")]
	pub connect: Option<Url>,

	/// Bind an HTTP listener for WHIP/WHEP.
	#[arg(id = "rtc-listen", long = "listen", value_name = "ADDR")]
	pub listen: Option<SocketAddr>,

	/// Shared UDP socket for ICE/media (one port for all sessions).
	#[arg(long, requires = "rtc-listen", default_value = "[::]:0")]
	pub udp_bind: SocketAddr,

	/// Public UDP address(es) advertised as ICE host candidates (repeatable).
	#[arg(long, requires = "rtc-listen")]
	pub public_addr: Vec<SocketAddr>,
}
