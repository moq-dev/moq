//! The unified moq-cli argument surface.
//!
//! Grammar: `moq <MoQ side> <import|export> <endpoint> [endpoint opts]`.
//!
//! - The MoQ side (`--client-connect` / `--server-bind`, both optional, at least
//!   one) attaches the shared Origin to the MoQ network, and comes before the
//!   verb. Both may be given: dial a relay *and* accept incoming sessions.
//! - `import` routes media INTO MoQ from one source; `export` routes it OUT to
//!   one sink. The verb fixes the data direction (and thus, for the
//!   bidirectional gateways, whether `--connect`/`--listen` push or pull).
//! - The endpoint is one subcommand: a container format (`ts`, `fmp4`, ... read
//!   from stdin on import, written to stdout on export) or a gateway (`hls`,
//!   `rtmp`, `srt`, `rtc`). Exactly one per invocation, so "which endpoint" is
//!   unambiguous and there's no silently-ignored flag.

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

	/// The MoQ attachment, shared by both directions.
	#[command(flatten)]
	pub moq: MoqSide,

	#[command(subcommand)]
	pub direction: Direction,
}

/// The MoQ attachment. At least one of `--client-connect` / `--server-bind`;
/// both may be given at once.
#[derive(Args, Clone)]
#[command(group = ArgGroup::new("moq").required(true).multiple(true).args(["client-connect", "server-bind"]))]
pub struct MoqSide {
	/// Dial a MoQ relay/server over WebTransport.
	///
	/// The URL path is the relay auth path (e.g. `/anon` for a public relay); the
	/// broadcast rides on top of it (via `--broadcast` or the endpoint). `?jwt=`
	/// supplies a token. `http://` first fetches `/certificate.sha256` for the
	/// (insecure) self-signed fingerprint; `https://` connects directly.
	#[arg(
		id = "client-connect",
		long = "client-connect",
		env = "MOQ_CLIENT_CONNECT",
		help_heading = "MoQ"
	)]
	pub client_connect: Option<Url>,

	/// The broadcast name for a single-broadcast endpoint (stdin/stdout, HLS, and
	/// the foreign `--connect` dials). Optional: MoQ names each broadcast by the
	/// connection path plus this name, so leaving it unset uses the root broadcast
	/// at the connection path. Ignored by the dynamic (`--listen`) endpoints,
	/// which name broadcasts from the protocol (RTMP app/key, SRT stream id).
	#[arg(long, alias = "name", help_heading = "MoQ")]
	pub broadcast: Option<String>,

	/// When hosting (`--server-bind`), also serve static files and the
	/// `/certificate.sha256` fingerprint over HTTP from this directory.
	#[arg(long, requires = "server-bind", help_heading = "MoQ")]
	pub dir: Option<PathBuf>,

	/// MoQ client transport config (`--client-bind`, `--client-tls-*`, ...).
	#[command(flatten)]
	pub client: moq_native::ClientConfig,

	/// MoQ server transport config (`--server-bind`, `--server-tls-*`, `--tls-*`).
	#[command(flatten)]
	pub server: moq_native::ServerConfig,
}

/// The data direction: the pivot between the MoQ side and the endpoint.
#[derive(Subcommand, Clone)]
pub enum Direction {
	/// Route media INTO MoQ from one source.
	Import(Import),
	/// Route media OUT OF MoQ to one sink.
	Export(Export),
}

// ------------------------------------------------------------------ import

/// import = one source -> MoQ.
#[derive(Args, Clone)]
pub struct Import {
	#[command(subcommand)]
	pub source: ImportSource,
}

/// The single source feeding the Origin on an import. The container formats read
/// from stdin; the gateways bridge another protocol.
#[derive(Subcommand, Clone)]
pub enum ImportSource {
	/// Raw H.264 Annex-B from stdin.
	Avc3,
	/// Fragmented MP4 / CMAF from stdin.
	Fmp4,
	/// MPEG-TS from stdin.
	Ts,
	/// FLV / RTMP container from stdin.
	Flv,
	/// Pull a remote HLS / LL-HLS playlist (http/https URL or local file) into MoQ.
	Hls {
		/// Playlist URL or local file path.
		playlist: String,
	},
	/// RTMP: pull a remote play (`--connect`) or accept incoming publishes (`--listen`).
	Rtmp(crate::rtmp::Args),
	/// SRT: pull a remote stream (`--connect`) or accept incoming publishes (`--listen`).
	Srt(crate::srt::Args),
	/// WebRTC: WHEP client pulling a remote (`--connect`) or WHIP server accepting publishes (`--listen`).
	Rtc(crate::rtc::Args),
}

impl ImportSource {
	/// The stdin container format, when this source is one of the container formats.
	pub fn stdin_format(&self) -> Option<PublishFormat> {
		Some(match self {
			Self::Avc3 => PublishFormat::Avc3,
			Self::Fmp4 => PublishFormat::Fmp4,
			Self::Ts => PublishFormat::Ts,
			Self::Flv => PublishFormat::Flv,
			_ => return None,
		})
	}
}

// ------------------------------------------------------------------ export

/// export = MoQ -> one sink.
#[derive(Args, Clone)]
pub struct Export {
	/// Maximum latency before skipping groups (e.g. `500ms`, `1s`). Applies to the
	/// container formats written to stdout.
	#[arg(long, default_value = "500ms", value_parser = humantime::parse_duration)]
	pub max_latency: Duration,

	/// Cap the output fragment duration (e.g. `2s`). Default: one GOP. Applies to
	/// the fmp4 / mkv stdout formats.
	#[arg(long, value_parser = humantime::parse_duration)]
	pub fragment_duration: Option<Duration>,

	/// Catalog format for track discovery (default: detect from the broadcast suffix).
	#[arg(long)]
	pub catalog: Option<CatalogFormatArg>,

	#[command(subcommand)]
	pub sink: ExportSink,
}

/// The single sink draining the Origin on an export. The container formats write
/// to stdout; the gateways bridge another protocol.
#[derive(Subcommand, Clone)]
pub enum ExportSink {
	/// Fragmented MP4 / CMAF to stdout.
	Fmp4,
	/// Matroska / WebM to stdout.
	Mkv,
	/// MPEG-TS to stdout.
	Ts,
	/// FLV / RTMP container to stdout.
	Flv,
	/// Serve HLS / LL-HLS over HTTP.
	Hls(crate::hls::Args),
	/// RTMP: push to a remote (`--connect`) or serve plays (`--listen`).
	Rtmp(crate::rtmp::Args),
	/// SRT: push to a remote (`--connect`) or serve requests (`--listen`).
	Srt(crate::srt::Args),
	/// WebRTC: WHIP client pushing to a remote (`--connect`) or WHEP server serving plays (`--listen`).
	Rtc(crate::rtc::Args),
}

impl ExportSink {
	/// The stdout container format, when this sink is one of the container formats.
	pub fn stdout_format(&self) -> Option<SubscribeFormat> {
		Some(match self {
			Self::Fmp4 => SubscribeFormat::Fmp4,
			Self::Mkv => SubscribeFormat::Mkv,
			Self::Ts => SubscribeFormat::Ts,
			Self::Flv => SubscribeFormat::Flv,
			_ => return None,
		})
	}
}
