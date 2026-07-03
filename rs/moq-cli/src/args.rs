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

use std::time::Duration;

use clap::{ArgGroup, Args, Parser, Subcommand};
use url::Url;

use crate::publish::PublishFormat;
use crate::subscribe::{CatalogFormatArg, SubscribeFormat};

/// moq-cli: a media router that wires one endpoint onto a shared MoQ Origin.
#[derive(Parser, Clone)]
#[command(name = "moq", version = env!("VERSION"))]
pub struct Cli {
	/// Logging configuration.
	#[command(flatten)]
	pub log: moq_native::Log,

	/// The MoQ attachment, shared by both directions.
	#[command(flatten)]
	pub moq: MoqSide,

	/// The routing direction and endpoint.
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

	/// The broadcast name. Optional for the point endpoints (stdin/stdout, HLS
	/// import, and the `--connect` dials), which default to the root broadcast at
	/// the connection path; required by the `--listen` endpoints and `hls export`,
	/// which bridge one named broadcast.
	#[arg(long, alias = "name", help_heading = "MoQ")]
	pub broadcast: Option<String>,

	/// MoQ client transport config (`--client-bind`, `--client-tls-*`, ...).
	#[command(flatten)]
	pub client: moq_native::ClientConfig,

	/// MoQ server transport config (`--server-bind`, `--server-tls-*`, `--tls-*`).
	#[command(flatten)]
	pub server: moq_native::ServerConfig,

	/// Iroh transport config (`--iroh-*`), used by both the client and server.
	#[cfg(feature = "iroh")]
	#[command(flatten)]
	pub iroh: moq_native::iroh::EndpointConfig,
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
	/// The single source feeding the Origin.
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
	Hls(crate::hls::ImportArgs),
	/// RTMP: pull a remote play (`--connect`) or accept incoming publishes (`--listen`).
	Rtmp(crate::rtmp::Args),
	/// SRT: pull a remote stream (`--connect`) or accept incoming publishes (`--listen`).
	Srt(crate::srt::Args),
	/// WebRTC: WHEP client pulling a remote (`--connect`) or WHIP server accepting publishes (`--listen`).
	Rtc(crate::rtc::Args),
	/// Capture the camera/microphone (or screen) and encode natively.
	#[cfg(feature = "capture")]
	Capture(crate::publish::CaptureArgs),
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
	/// Catalog format to read for track discovery (default: detect from the broadcast suffix).
	#[arg(long = "catalog-format")]
	pub catalog_format: Option<CatalogFormatArg>,

	/// Rendition selection (`--video-name`, `--video-codec`, `--audio-name`, `--audio-codec`).
	#[command(flatten)]
	pub select: crate::subscribe::SelectArgs,

	/// The single sink draining the Origin.
	#[command(subcommand)]
	pub sink: ExportSink,
}

/// The single sink draining the Origin on an export. The container formats write
/// to stdout; the gateways bridge another protocol.
#[derive(Subcommand, Clone)]
pub enum ExportSink {
	/// Fragmented MP4 / CMAF to stdout.
	Fmp4(Fragmented),
	/// Matroska / WebM to stdout.
	Mkv(Fragmented),
	/// MPEG-TS to stdout.
	Ts(Container),
	/// FLV / RTMP container to stdout.
	Flv(Container),
	/// H.264 Annex-B elementary stream to stdout.
	H264(Container),
	/// H.265 Annex-B elementary stream to stdout.
	H265(Container),
	/// Serve HLS / LL-HLS over HTTP.
	Hls(crate::hls::ExportArgs),
	/// RTMP: push to a remote (`--connect`) or serve plays (`--listen`).
	Rtmp(crate::rtmp::ExportArgs),
	/// SRT: push to a remote (`--connect`) or serve requests (`--listen`).
	Srt(crate::srt::Args),
	/// WebRTC: WHIP client pushing to a remote (`--connect`) or WHEP server serving plays (`--listen`).
	Rtc(crate::rtc::Args),
}

impl ExportSink {
	/// The stdout container format plus its latency and fragment cap, when this
	/// sink writes to stdout (the container formats). The fragment cap is
	/// fmp4/mkv-only.
	pub fn stdout(&self) -> Option<(SubscribeFormat, Duration, Option<Duration>)> {
		Some(match self {
			Self::Fmp4(args) => (
				SubscribeFormat::Fmp4,
				args.container.latency_max,
				args.fragment_duration,
			),
			Self::Mkv(args) => (SubscribeFormat::Mkv, args.container.latency_max, args.fragment_duration),
			Self::Ts(args) => (SubscribeFormat::Ts, args.latency_max, None),
			Self::Flv(args) => (SubscribeFormat::Flv, args.latency_max, None),
			Self::H264(args) => (SubscribeFormat::H264, args.latency_max, None),
			Self::H265(args) => (SubscribeFormat::H265, args.latency_max, None),
			_ => return None,
		})
	}
}

/// Options shared by every stdout container sink.
#[derive(Args, Clone)]
pub struct Container {
	/// Maximum latency before skipping a stalled group (e.g. `500ms`, `1s`).
	#[arg(long = "latency-max", default_value = "500ms", value_parser = humantime::parse_duration)]
	pub latency_max: Duration,
}

/// The fmp4 / mkv stdout containers: [`Container`] plus a fragment cap.
#[derive(Args, Clone)]
pub struct Fragmented {
	#[command(flatten)]
	pub container: Container,

	/// Cap the output fragment/cluster duration (e.g. `2s`). Default: one GOP.
	#[arg(long, value_parser = humantime::parse_duration)]
	pub fragment_duration: Option<Duration>,
}
