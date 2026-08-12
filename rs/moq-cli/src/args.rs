//! The unified moq-cli argument surface.
//!
//! Grammar: `moq <MoQ side> <import|export> <endpoint> [endpoint opts]`, plus
//! `moq <MoQ side> play` for native playback.
//!
//! - The MoQ side (`--connect`, `--listen`, `--cluster-lan`; all
//!   optional, at least one) attaches the shared Origin to the MoQ network, and
//!   comes before the verb. They compose: dial a relay, accept incoming
//!   sessions, and mesh with the LAN all at once.
//! - `import` routes media INTO MoQ from one source; `export` routes it OUT to
//!   one sink. The verb fixes the data direction (and thus, for the
//!   bidirectional gateways, whether `--connect`/`--listen` push or pull).
//! - `devices` and `token` touch no network at all, so they're the verbs that take
//!   no MoQ side. That's why the requirement is enforced per-verb
//!   ([`MoqSide::validate`]) rather than by clap: an `ArgGroup` can't be
//!   conditional on the subcommand.
//! - The endpoint is one subcommand: a container format (`ts`, `fmp4`, ... read
//!   from stdin on import, written to stdout on export) or a gateway (`hls`,
//!   `rtmp`, `srt`, `rtc`). Exactly one per invocation, so "which endpoint" is
//!   unambiguous and there's no silently-ignored flag.

use std::time::Duration;

use clap::{ArgGroup, Args, Parser, Subcommand};
use hang::moq_net;

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

	/// The verb and endpoint.
	#[command(subcommand)]
	pub command: Command,
}

/// The MoQ attachment: a relay dial, a server listener, a LAN mesh, or any
/// combination.
///
/// The group is not `required`, because the local verbs (`token`, `devices`) run
/// without a MoQ side. Every verb that does need one calls
/// [`validate`](Self::validate).
#[derive(Args, Clone)]
#[cfg_attr(
	feature = "cluster-lan",
	command(group = ArgGroup::new("moq").multiple(true).args(["connect", "listen", "cluster-lan"]))
)]
#[cfg_attr(
	not(feature = "cluster-lan"),
	command(group = ArgGroup::new("moq").multiple(true).args(["connect", "listen"]))
)]
pub struct MoqSide {
	/// The broadcast name. Optional for the point endpoints (stdin/stdout, HLS
	/// import, and the `--connect` dials), which default to the root broadcast at
	/// the connection path; required by the `--listen` endpoints and `hls export`,
	/// which bridge one named broadcast.
	#[arg(long, alias = "name", help_heading = "MoQ")]
	pub broadcast: Option<String>,

	/// Fix this process's origin id instead of minting a fresh random one.
	///
	/// The origin id is the first hop of every announcement this process
	/// publishes, and relays treat it as the broadcast's content identity:
	/// redundant publishers of the same broadcast share an id so relays fail
	/// over between them at a group boundary. Leave unset outside a redundant
	/// (1+1) chain; the default fresh id per run is what makes a restarted
	/// publisher look like new content instead of silently splicing.
	#[arg(long, env = "MOQ_ORIGIN", help_heading = "MoQ")]
	pub origin: Option<u64>,

	/// MoQ client config (`--connect`, `--connect-bind`, `--connect-tls-*`, ...).
	#[command(flatten)]
	pub client: moq_native::connect::Config,

	/// QUIC transport tuning (`--quic-*`), shared by the dial and accept sides.
	#[command(flatten)]
	pub quic: moq_native::quic::Config,

	/// MoQ server transport config (`--listen`, `--listen-tls-*`).
	#[command(flatten)]
	pub server: moq_native::listen::Config,

	/// Iroh transport config (`--iroh-*`), used by both the client and server.
	#[cfg(feature = "iroh")]
	#[command(flatten)]
	pub iroh: moq_native::iroh::EndpointConfig,

	/// LAN clustering config (`--cluster-lan`, `--cluster-lan-secret`).
	#[cfg(feature = "cluster-lan")]
	#[command(flatten)]
	pub cluster: crate::cluster::Args,
}

impl MoqSide {
	/// Mint the origin all broadcasts route through: the pinned `--origin` id
	/// when set, otherwise fresh and random.
	pub fn origin(&self) -> anyhow::Result<moq_net::origin::Producer> {
		use anyhow::Context;
		Ok(match self.origin {
			Some(id) => moq_net::Origin::new(id).with_context(|| format!("invalid --origin {id}"))?,
			None => moq_net::Origin::random(),
		}
		.produce())
	}

	/// Whether `--cluster-lan` asked this process to mesh over the LAN.
	pub fn lan(&self) -> bool {
		#[cfg(feature = "cluster-lan")]
		return self.cluster.enabled();
		#[cfg(not(feature = "cluster-lan"))]
		false
	}

	/// The server to bind, which the LAN mesh shares with ordinary clients.
	///
	/// `--cluster-lan` needs a listener for peers to dial, so it fills in the two
	/// things the user would otherwise have to spell out: an ephemeral port and a
	/// generated certificate. An explicit `--listen` or `--listen-tls-*` wins, which
	/// is what puts the mesh on the same port and certificate as everything else.
	pub fn server_config(&self) -> moq_native::listen::Config {
		let mut config = self.server.resolved();
		if self.lan() {
			config.bind.get_or_insert_with(|| "[::]:0".to_string());
			if config.tls.generate.is_empty() && config.tls.cert.is_empty() {
				config.tls.generate = vec!["moq-cluster-lan".to_string()];
			}
		}
		config
	}

	/// Whether a listener has to be bound at all.
	pub fn serves(&self) -> bool {
		self.server.resolved().bind.is_some() || self.lan()
	}

	/// Reject a verb that needs the MoQ network but was given no way to reach it.
	/// Stands in for the clap `required` the `moq` group can't carry, since
	/// `devices` is exempt.
	pub fn validate(&self) -> anyhow::Result<()> {
		anyhow::ensure!(
			self.client.resolved().url.is_some() || self.serves(),
			"a MoQ side is required: pass --connect <url> to dial a relay, --listen <addr> to self-host, or --cluster-lan to mesh over the LAN"
		);
		#[cfg(feature = "cluster-lan")]
		{
			self.cluster.validate()?;
			if self.lan() {
				crate::cluster::validate_versions(&self.client.resolved(), &self.server_config())?;
			}
		}
		Ok(())
	}

	/// Reject the MoQ flags on a verb that never touches the network, rather than
	/// silently ignoring them. `--broadcast` counts: a local verb has no content, and
	/// next to `token generate` it reads like it scopes the key, which `--root` does.
	///
	/// `--origin` is left out on purpose. It reads `MOQ_ORIGIN`, so rejecting it would
	/// fail `moq token` in any shell that exports the variable for a publisher, and an
	/// ambient env value is not the deliberate request this is meant to catch.
	pub fn reject(&self, command: &str) -> anyhow::Result<()> {
		#[cfg(feature = "cluster-lan")]
		let cluster_secret = self.cluster.secret.is_some();
		#[cfg(not(feature = "cluster-lan"))]
		let cluster_secret = false;

		// Read through the fold: a legacy `--client-connect` must be rejected here
		// too, and it only lands in `url` once resolved.
		let connect = self.client.resolved();
		let listen = self.server.resolved();
		let ignored = [
			("--connect", connect.url.is_some()),
			("--listen", listen.bind.is_some()),
			("--cluster-lan", self.lan()),
			("--cluster-lan-secret", cluster_secret),
			("--broadcast", self.broadcast.is_some()),
		];

		if let Some((flag, _)) = ignored.into_iter().find(|(_, given)| *given) {
			anyhow::bail!("`{command}` runs locally and takes no MoQ side; drop {flag}");
		}

		Ok(())
	}
}

/// The verb: for `import`/`export` it is also the data direction, the pivot
/// between the MoQ side and the endpoint.
#[derive(Subcommand, Clone)]
pub enum Command {
	/// Route media INTO MoQ from one source.
	#[command(alias = "publish")]
	Import(Import),
	/// Route media OUT OF MoQ to one sink.
	#[command(alias = "subscribe")]
	Export(Export),
	/// Play a broadcast in a native window and speaker.
	#[cfg(feature = "play")]
	Play(crate::play::Args),
	/// Re-encode `--broadcast` into a lower ladder, published next to it and
	/// only encoded while watched (just-in-time).
	#[cfg(feature = "transcode")]
	Transcode(crate::transcode::Args),
	/// Generate, sign, and verify the JWT tokens a relay authenticates with.
	Token(moq_token_cli::Args),
	/// List the capture devices `import capture` can name.
	#[cfg(feature = "capture")]
	Devices,
}

// ------------------------------------------------------------------ import

/// import = one source -> MoQ.
#[derive(Args, Clone)]
pub struct Import {
	/// How long relays keep a non-latest group of the published media tracks fetchable,
	/// e.g. "30s" or "5s". Defaults to hang's 30s.
	///
	/// A RETENTION budget, not a delivery one: it never makes a subscriber play further behind
	/// live, it caps how far back a FETCH can still reach (and how long a subscriber may ask to
	/// wait for a late group). The default suits a segmented egress (HLS/DASH), which may only
	/// advertise segments that are still fetchable; lower it when nothing reads history and the
	/// memory matters. Media tracks only -- the catalog and timeline are read at the live edge,
	/// which is retained unconditionally.
	#[arg(long, value_parser = humantime::parse_duration)]
	pub latency_max: Option<std::time::Duration>,

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
	/// Capture a local source (camera, display, window, app, microphone) and
	/// encode natively. Run `moq devices` to list them.
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
	/// Serve HLS / LL-HLS and DASH over HTTP.
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
	pub fn stdout(&self) -> Option<(SubscribeFormat, moq_mux::Latency, Option<Duration>)> {
		Some(match self {
			Self::Fmp4(args) => (SubscribeFormat::Fmp4, args.container.latency(), args.fragment_duration),
			Self::Mkv(args) => (SubscribeFormat::Mkv, args.container.latency(), args.fragment_duration),
			Self::Ts(args) => (SubscribeFormat::Ts, args.latency(), None),
			Self::Flv(args) => (SubscribeFormat::Flv, args.latency(), None),
			Self::H264(args) => (SubscribeFormat::H264, args.latency(), None),
			Self::H265(args) => (SubscribeFormat::H265, args.latency(), None),
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

impl Container {
	/// The configured latency tolerance.
	pub fn latency(&self) -> moq_mux::Latency {
		moq_mux::Latency::max(self.latency_max)
	}
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

#[cfg(test)]
mod tests {
	use super::*;
	use clap::CommandFactory;

	// Catches the conflicts clap only panics on at runtime: a duplicate long, a
	// dangling `conflicts_with`, a flattened arg colliding with an existing one.
	// The token verb flattens a whole command tree from another crate, so this is
	// the only thing standing between a rename there and a broken `moq`.
	#[test]
	fn valid() {
		Cli::command().debug_assert();
	}

	#[test]
	fn latency_max_is_unset_unless_asked_for() {
		// Unset rather than defaulted to hang's constant, so the publisher's own default is
		// what every source falls back to. A `default_value` here would put the number in the
		// CLI as well, and the two would drift.
		let cli = Cli::try_parse_from(["moq", "import", "ts"]).unwrap();
		let Command::Import(import) = cli.command else {
			panic!("expected import")
		};
		assert_eq!(import.latency_max, None);

		// It sits on the parent `import`, so it parses ahead of any source, gateway or not.
		let cli = Cli::try_parse_from(["moq", "import", "--latency-max", "5s", "ts"]).unwrap();
		let Command::Import(import) = cli.command else {
			panic!("expected import")
		};
		assert_eq!(import.latency_max, Some(std::time::Duration::from_secs(5)));

		let cli = Cli::try_parse_from([
			"moq",
			"import",
			"--latency-max",
			"5s",
			"rtmp",
			"--listen",
			"127.0.0.1:1935",
		])
		.unwrap();
		let Command::Import(import) = cli.command else {
			panic!("expected import")
		};
		assert_eq!(import.latency_max, Some(std::time::Duration::from_secs(5)));
	}

	#[test]
	fn token_verb() {
		let cli = Cli::try_parse_from(["moq", "token", "generate", "--algorithm", "ES256"]).unwrap();
		assert!(matches!(cli.command, Command::Token(_)));
		// Local verb: it needs no MoQ side, so what every other verb demands...
		assert!(cli.moq.validate().is_err());
		assert!(cli.moq.reject("token").is_ok());

		// ...these it refuses, rather than accepting the flag and ignoring it.
		for (flag, value, reported) in [
			("--connect", "https://relay.example.com", "--connect"),
			// The released spelling folds in, so it is rejected under its
			// canonical name rather than silently ignored.
			("--client-connect", "https://relay.example.com", "--connect"),
			("--broadcast", "room", "--broadcast"),
		] {
			let cli = Cli::try_parse_from(["moq", flag, value, "token", "generate"]).unwrap();
			let err = cli.moq.reject("token").unwrap_err().to_string();
			assert!(err.contains(reported), "{err}");
		}

		#[cfg(feature = "cluster-lan")]
		{
			let cli = Cli::try_parse_from(["moq", "--cluster-lan", "token", "generate"]).unwrap();
			let err = cli.moq.reject("token").unwrap_err().to_string();
			assert!(err.contains("--cluster-lan"), "{err}");

			// Clap considers the secret's `requires` satisfied when the boolean flag
			// is explicitly present but false. The local verb still has to reject the
			// otherwise silently ignored secret.
			let cli = Cli::try_parse_from([
				"moq",
				"--cluster-lan=false",
				"--cluster-lan-secret",
				"cluster.key",
				"token",
				"generate",
			])
			.unwrap();
			let err = cli.moq.reject("token").unwrap_err().to_string();
			assert!(err.contains("--cluster-lan-secret"), "{err}");
		}
	}

	/// `--cluster-lan` is a MoQ side on its own, and it supplies the listener the
	/// user would otherwise have to spell out.
	#[cfg(feature = "cluster-lan")]
	#[test]
	fn cluster_lan_is_a_moq_side_and_fills_in_a_listener() {
		let cli = Cli::try_parse_from(["moq", "--cluster-lan", "import", "ts"]).expect("parse");
		assert!(cli.moq.lan());
		assert!(cli.moq.validate().is_ok(), "the LAN mesh is a MoQ side on its own");

		let server = cli.moq.server_config();
		assert_eq!(server.bind.as_deref(), Some("[::]:0"), "an ephemeral port");
		assert_eq!(server.tls.generate, ["moq-cluster-lan"], "a generated certificate");

		// An explicit listener wins, so the mesh shares one port and certificate
		// with ordinary clients.
		let cli = Cli::try_parse_from([
			"moq",
			"--cluster-lan",
			"--server-bind",
			"[::]:4443",
			"--tls-generate",
			"localhost",
			"import",
			"ts",
		])
		.expect("parse");
		let server = cli.moq.server_config();
		assert_eq!(server.bind.as_deref(), Some("[::]:4443"));
		assert_eq!(server.tls.generate, ["localhost"]);

		// Without the mesh, nothing is filled in.
		let cli = Cli::try_parse_from(["moq", "--client-connect", "https://relay.example.com", "import", "ts"])
			.expect("parse");
		assert!(!cli.moq.lan());
		assert_eq!(cli.moq.server_config().bind, None);
	}

	/// The secret is only read by the mesh, so configuring one without it is an
	/// error rather than a silently ignored flag.
	#[cfg(feature = "cluster-lan")]
	#[test]
	fn cluster_lan_secret_requires_the_mesh() {
		let err = Cli::try_parse_from(["moq", "--cluster-lan-secret", "cluster.key", "import", "ts"])
			.err()
			.expect("the secret must require --cluster-lan")
			.to_string();
		assert!(err.contains("--cluster-lan"), "{err}");

		// `--cluster-lan=false` satisfies clap's `requires` (the flag is present),
		// so the real check lives in `validate`.
		let cli = Cli::try_parse_from([
			"moq",
			"--cluster-lan=false",
			"--cluster-lan-secret",
			"cluster.key",
			"--client-connect",
			"https://relay.example.com",
			"import",
			"ts",
		])
		.expect("parse");
		let err = cli.moq.validate().unwrap_err().to_string();
		assert!(err.contains("--cluster-lan=true"), "{err}");

		let cli = Cli::try_parse_from([
			"moq",
			"--cluster-lan",
			"--cluster-lan-secret",
			"cluster.key",
			"import",
			"ts",
		])
		.expect("parse");
		assert!(cli.moq.validate().is_ok());
		assert_eq!(cli.moq.cluster.secret.as_deref(), Some("cluster.key"));
	}

	/// A mesh dial authenticates through its request path, which legacy moq-lite
	/// versions do not carry.
	#[cfg(feature = "cluster-lan")]
	#[test]
	fn cluster_lan_requires_a_path_capable_version() {
		// Both the canonical spellings and the released ones, which fold in.
		for (flag, reported) in [
			("--connect-version", "--connect-version"),
			("--listen-version", "--listen-version"),
			("--client-version", "--connect-version"),
			("--server-version", "--listen-version"),
		] {
			let cli =
				Cli::try_parse_from(["moq", "--cluster-lan", flag, "moq-lite-04", "import", "ts"]).expect("parse");
			let err = cli.moq.validate().unwrap_err().to_string();
			assert!(err.contains(reported), "{flag}: {err}");
		}

		let cli = Cli::try_parse_from([
			"moq",
			"--cluster-lan",
			"--client-version",
			"moq-lite-04",
			"--client-version",
			"moq-lite-05",
			"--server-version",
			"moq-lite-05",
			"import",
			"ts",
		])
		.expect("parse");
		assert!(cli.moq.validate().is_ok());
	}

	#[cfg(feature = "play")]
	#[test]
	fn play_verb() {
		let cli = Cli::try_parse_from([
			"moq",
			"--client-connect",
			"https://relay.example.com/anon",
			"--broadcast",
			"room.hang",
			"play",
			"--video-name",
			"hd",
		])
		.unwrap();
		let Command::Play(play) = cli.command else {
			panic!("expected play")
		};
		assert_eq!(play.latency_max, Duration::from_millis(500));
		assert_eq!(play.select.video_name.as_deref(), Some("hd"));
		assert!(cli.moq.validate().is_ok());
		assert!(play.validate().is_ok());
	}

	/// The selection flags are shared with the exports, which pass every codec
	/// through. Playback has to decode, so it rejects the rest up front instead
	/// of filtering the catalog down to a rendition that can't open.
	#[cfg(feature = "play")]
	#[test]
	fn play_rejects_undecodable_codecs() {
		for flag in [["--video-codec", "vp9"], ["--audio-codec", "aac"]] {
			let cli = Cli::try_parse_from([
				"moq",
				"--client-connect",
				"https://relay.example.com/anon",
				"play",
				flag[0],
				flag[1],
			])
			.unwrap();
			let Command::Play(play) = cli.command else {
				panic!("expected play")
			};
			let err = play.validate().unwrap_err().to_string();
			assert!(err.contains(flag[1]), "{err}");
		}
	}
}
