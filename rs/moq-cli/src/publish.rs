use anyhow::Context;
use moq_mux::container::{flv, fmp4, ts};

/// Container format read from stdin on the import (source) side.
#[derive(Clone, Copy)]
pub enum PublishFormat {
	/// Raw AVC (H.264) Annex B elementary stream.
	Avc3,
	/// Fragmented MP4 (CMAF), republished whole as CMAF when `cmaf`.
	Fmp4 { cmaf: bool },
	/// MPEG-TS (transport stream), limited to one `program` of a multiplex when given.
	Ts { program: Option<u16> },
	/// MPEG-TS published whole, paced on `pcr_pid` when given rather than the PMT's clock.
	TsPassthrough { pcr_pid: Option<u16> },
	/// FLV (Flash Video / RTMP).
	Flv,
}

/// Command-line adapter for [`moq_video::encode::Codec`].
#[cfg(feature = "capture")]
#[derive(usage_rs::ValueEnum, Clone, Copy, Default)]
pub enum VideoCodec {
	/// H.264 / AVC (the default; widest support).
	#[default]
	H264,
	/// H.265 / HEVC (hardware-only).
	H265,
}

#[cfg(feature = "capture")]
impl From<VideoCodec> for moq_video::encode::Codec {
	fn from(codec: VideoCodec) -> Self {
		match codec {
			VideoCodec::H264 => moq_video::encode::Codec::H264,
			VideoCodec::H265 => moq_video::encode::Codec::H265,
		}
	}
}

/// When a captured medium produces a track.
#[cfg(feature = "capture")]
#[derive(usage_rs::ValueEnum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
	/// Capture and encode for the whole publish.
	On,
	/// Encode only while subscribed. Video captures only then too; audio keeps
	/// capturing and retains raw PCM up to `--max-age`, encoding it once subscribed.
	Demand,
	/// Publish no track of this medium.
	Off,
}

/// Device capture options. Video (camera/screen -> H.264/H.265) maps to
/// `moq-video`; audio (microphone/system -> Opus) to `moq-audio`. Both are
/// captured by default; use `--video off` / `--audio off` to publish only one.
///
/// The video source is one of `--camera` / `--display` / `--window` / `--app`,
/// and the audio source one of `--microphone` / `--system-audio`, defaulting to
/// the default camera and microphone. Run `moq devices` to list the ids each one
/// takes.
#[cfg(feature = "capture")]
#[derive(usage_rs::Args, Clone)]
#[usage(unknown_flags = "error", args_override_self = false)]
#[usage(group("video-source"))]
#[usage(group("audio-source"))]
pub struct CaptureArgs {
	/// Capture a camera, by the id `moq devices` reports (an AVFoundation
	/// `uniqueID`, `/dev/videoN` path, `pipewire:` node, or Media Foundation
	/// symbolic link).
	/// Bare `--camera`, or no source flag at all, opens the default camera.
	#[usage(long, group = "video-source")]
	pub camera: Option<Option<String>>,

	/// Capture a whole display, by the id `moq devices` reports. Bare
	/// `--display` captures the main display, or on Wayland opens the system
	/// portal picker (requires the `pipewire` feature). Wayland accepts only
	/// the XWayland `x11:` ids it lists.
	#[usage(long, group = "video-source", alias = "screen")]
	pub display: Option<Option<String>>,

	/// Capture a single window, by the id `moq devices` reports on macOS,
	/// Windows, or X11. Bare `--window` opens the Linux system portal picker
	/// (requires the `pipewire` feature); no window id is needed.
	#[usage(long, group = "video-source")]
	pub window: Option<Option<String>>,

	/// Capture every window of an application, by the bundle id `moq devices`
	/// reports. Windows opened later are included. macOS only.
	#[usage(long, group = "video-source")]
	pub app: Option<String>,

	/// Hide the mouse cursor. Display/window/app capture only.
	#[usage(long)]
	pub no_cursor: bool,

	/// Requested capture width. The source snaps to its nearest supported mode.
	#[usage(long)]
	pub width: Option<u32>,

	/// Requested capture height.
	#[usage(long)]
	pub height: Option<u32>,

	/// Capture/encode framerate. Omit to use the source's reported rate.
	#[usage(long)]
	pub fps: Option<u32>,

	/// Maximum video bitrate in bits per second. Omit to derive one from the resolution.
	///
	/// When publishing to a relay, the encoder backs off below this while the uplink is
	/// congested and climbs back afterwards; it never encodes above it.
	#[usage(long)]
	pub bitrate: Option<u64>,

	/// Video codec to encode. H.265 is hardware-only (VideoToolbox on macOS).
	#[usage(long, value_enum, default = "h264")]
	pub codec: VideoCodec,

	/// Force a hardware encoder (error if none is available).
	#[usage(long, conflicts = "--software")]
	pub hardware: bool,

	/// Force the software encoder (openh264).
	#[usage(long)]
	pub software: bool,

	/// Capture a microphone, by the id `moq devices` reports. Bare
	/// `--microphone`, or no audio source flag, opens the default input. It is
	/// encoded as voice, which sends nothing while its talker is silent.
	#[usage(long, group = "audio-source")]
	pub microphone: Option<Option<String>>,

	/// Capture the system (desktop) audio instead of a microphone: everything the
	/// machine is playing, minus this process. macOS only, and it needs the Screen
	/// Recording permission. It is encoded as music.
	#[usage(long, group = "audio-source")]
	pub system_audio: bool,

	/// Target audio bitrate in bits per second (Opus). Omit for the codec default.
	#[usage(long)]
	pub audio_bitrate: Option<u32>,

	/// Video mode: on keeps the recent past; demand captures only while subscribed;
	/// off publishes no video. Video probes its source once at startup even in demand mode.
	#[usage(long, value_enum, default = "on")]
	pub video: Mode,

	/// Audio mode: demand (the default) keeps the microphone open and retains raw
	/// audio, encoding it with the recent past once subscribed; on encodes for the
	/// whole publish; off publishes no audio.
	#[usage(long, value_enum, default = "demand")]
	pub audio: Mode,

	/// The released spelling of `--video off`.
	#[usage(long = "no-video", hide = true)]
	no_video: bool,

	/// The released spelling of `--audio off`.
	#[usage(long = "no-audio", hide = true)]
	no_audio: bool,

	/// The render node the encoder opens, such as `/dev/dri/renderD129`: VA-API
	/// opens it, and NVENC the NVIDIA GPU behind it. Defaults to the first that
	/// encodes, decodes, and scales. Linux only.
	#[usage(long, env = "MOQ_VAAPI_DEVICE", value_hint = usage_rs::ValueHint::FilePath, conflicts = "--v4l2-encoder")]
	pub vaapi_device: Option<std::path::PathBuf>,

	/// The V4L2 M2M encoder node, such as `/dev/video11`. Defaults to the first
	/// that encodes H.264. Linux only.
	#[usage(long, env = "MOQ_V4L2_ENCODER", value_hint = usage_rs::ValueHint::FilePath)]
	pub v4l2_encoder: Option<std::path::PathBuf>,
}

enum PublishDecoder {
	Avc3 {
		split: Box<moq_mux::codec::h264::Split>,
		import: Box<moq_mux::codec::h264::Import>,
	},
	Fmp4(Box<fmp4::Import>),
	/// `import ts`: one program, owning its route and catalog, republished on each change of
	/// source.
	Ts(Box<ts::Republish>),
	/// `import ts --program all`: one importer, broadcast, and catalog per program.
	TsPrograms(Box<ts::Programs>),
	/// `import ts --passthrough`: the multiplex whole, on one track.
	TsPassthrough(Box<ts::Passthrough>),
	Flv(Box<flv::Import>),
}

impl PublishDecoder {
	/// Decode a chunk of stdin bytes. Each importer buffers any partial trailing
	/// frame internally, so the caller feeds fresh chunks rather than an
	/// accumulating buffer.
	///
	/// A TS import whose source changed returns the change before reading on; see
	/// [`ts::Republish::decode`].
	fn decode_chunk(&mut self, chunk: &[u8]) -> anyhow::Result<Option<ts::Republished>> {
		match self {
			Self::Avc3 { split, import } => {
				let frames = split.decode(chunk, None)?;
				import.decode(frames)?;
			}
			Self::Fmp4(d) => d.decode(chunk)?,
			Self::Ts(d) => return d.decode(chunk).map_err(suggest_program),
			Self::TsPrograms(d) => d.decode(chunk)?,
			Self::TsPassthrough(d) => d.decode(chunk)?,
			Self::Flv(d) => d.decode(chunk)?,
		}
		Ok(None)
	}

	/// What each elementary stream has delivered and the audio frame sync lost so far, for
	/// the formats that report it.
	fn stats(&self) -> Option<ts::stats::Snapshot> {
		match self {
			Self::Ts(d) => Some(d.stats()),
			Self::TsPrograms(d) => Some(d.stats()),
			Self::Avc3 { .. } | Self::Fmp4(_) | Self::TsPassthrough(_) | Self::Flv(_) => None,
		}
	}

	/// Flush any buffered trailing frame and close the tracks at end of input.
	/// The avc3 split holds the final access unit until the next start code, so
	/// stdin EOF must flush it explicitly.
	fn finish(self) -> anyhow::Result<FinishedDecoder> {
		Ok(match self {
			Self::Avc3 { mut split, mut import } => {
				let tail = split.flush(None)?;
				import.decode(tail)?;
				FinishedDecoder::Avc3(Box::new(import.finish()?))
			}
			Self::Fmp4(d) => FinishedDecoder::Fmp4(Box::new(d.finish()?)),
			Self::Ts(d) => FinishedDecoder::Ts(Box::new(d.finish()?)),
			Self::TsPrograms(d) => FinishedDecoder::TsPrograms(Box::new(d.finish()?)),
			Self::TsPassthrough(d) => FinishedDecoder::TsPassthrough(Box::new(d.finish()?)),
			Self::Flv(d) => FinishedDecoder::Flv(Box::new(d.finish()?)),
		})
	}

	/// Abort the tracks with `err` instead of finishing, so subscribers see the
	/// real cause rather than `Error::Dropped`. Consumes the decoder.
	fn abort(self, err: moq_net::Error) {
		match self {
			Self::Avc3 { import, .. } => import.abort(err),
			Self::Fmp4(d) => d.abort(err),
			Self::Ts(d) => d.abort(err),
			Self::TsPrograms(d) => d.abort(err),
			Self::TsPassthrough(d) => d.abort(err),
			Self::Flv(d) => d.abort(err),
		}
	}
}

/// A [`PublishDecoder`] whose tracks finished, holding them so they stay servable.
enum FinishedDecoder {
	Avc3(Box<moq_mux::container::Finished<moq_mux::catalog::hang::Container, hang::catalog::VideoConfig>>),
	Fmp4(Box<fmp4::Finished>),
	Ts(Box<ts::republish::Finished>),
	TsPrograms(Box<ts::programs::Finished>),
	TsPassthrough(Box<ts::passthrough::Finished>),
	Flv(Box<flv::Finished>),
}

impl FinishedDecoder {
	/// The broadcasts a TS import owns; see [`ts::Republish::finish`] and [`ts::Programs::finish`].
	fn broadcasts(&self) -> Vec<moq_net::broadcast::Producer> {
		match self {
			Self::Ts(d) => vec![d.broadcast().clone()],
			Self::TsPrograms(d) => d.broadcasts(),
			_ => Vec::new(),
		}
	}

	/// What each elementary stream delivered, for the formats that report it.
	fn stats(&self) -> Option<ts::stats::Snapshot> {
		match self {
			Self::Ts(d) => Some(d.stats()),
			Self::TsPrograms(d) => Some(d.stats()),
			Self::Avc3(_) | Self::Fmp4(_) | Self::TsPassthrough(_) | Self::Flv(_) => None,
		}
	}

	fn abort(self, err: moq_net::Error) {
		match self {
			Self::Avc3(import) => import.abort(err),
			Self::Fmp4(d) => d.abort(err),
			Self::Ts(d) => d.abort(err),
			Self::TsPrograms(d) => d.abort(err),
			Self::TsPassthrough(d) => d.abort(err),
			Self::Flv(d) => d.abort(err),
		}
	}
}

/// Point a multi-program refusal at the flag that resolves it.
fn suggest_program(err: ts::Error) -> anyhow::Error {
	let multiplex = matches!(err, ts::Error::MultiplePrograms { .. });
	let err = anyhow::Error::from(err);
	match multiplex {
		true => err.context("choose one with `--program <n>`, or publish each with `--program all`"),
		false => err,
	}
}

/// The catalog a stdin decoder publishes into.
enum PublishCatalog {
	Media(Box<moq_mux::catalog::Producer>),
	/// A TS import's: each instance's or program's, with the `mpegts` extension, finished by
	/// [`ts::Republish::finish`] or [`ts::Programs::finish`].
	Ts,
}

impl PublishCatalog {
	fn abort(self, err: moq_net::Error) {
		match self {
			Self::Media(catalog) => catalog.abort(err),
			Self::Ts => {}
		}
	}

	/// End the catalog tracks cleanly, keeping the renditions they last listed.
	fn finish(&mut self) -> anyhow::Result<()> {
		match self {
			Self::Media(catalog) => catalog.finish()?,
			Self::Ts => {}
		}
		Ok(())
	}
}

#[cfg_attr(
	feature = "capture",
	expect(
		clippy::large_enum_variant,
		reason = "exactly one Source exists per process, so the gap between Stream and the larger Capture config is irrelevant"
	)
)]
enum Source {
	/// Decode a container read from stdin.
	Stream {
		decoder: PublishDecoder,
		catalog: PublishCatalog,
	},
	/// Capture from local devices. The per-medium producers are built on their
	/// own capture threads (native camera/screen capture, microphone via cpal), publishing
	/// onto the shared broadcast + catalog; [`Publish::run`] drives them
	/// concurrently.
	#[cfg(feature = "capture")]
	Capture {
		broadcast: moq_net::broadcast::Producer,
		catalog: moq_mux::catalog::Producer,
		video: Option<moq_video::encode::Capture>,
		audio: Option<(moq_audio::capture::Config, moq_audio::encode::Options, bool)>,
		/// Withholds the catalog until [`Publish::run`] starts the encoders, so the `stats` and
		/// `echo` sections set before then ride the first catalog rather than an empty one.
		gate: moq_mux::catalog::Reserved,
	},
}

/// A stdin or capture publisher: decodes stdin (or captures local devices) into
/// a broadcast that the MoQ side announces.
pub struct Publish {
	source: Source,
	// Keeps the fill surface until finish. `None` for `import ts --program all`,
	// whose programs each own their fill and route.
	broadcast: Option<moq_net::broadcast::Producer>,
	// The route announcing `broadcast`, held while it is up. Created hidden.
	route: Option<moq_net::origin::Route>,
	// Whether a stdin import announces once its catalog lists the tracks; see [`Self::announce`].
	announce: bool,
	// The `--report` track, written beside the decode loop; see [`Self::with_report`].
	report: Option<crate::report::Stats>,
	// How long a stdin import keeps its broadcast up after EOF; see [`Self::with_linger`].
	linger: std::time::Duration,
	// What a stdin TS import sets on each instance it republishes; see [`Self::stdin`].
	sections: Sections,
}

/// A broadcast's announcement, held until its catalog first goes out.
///
/// The catalog withholds its first snapshot until every reserved stream's config is known and
/// at least one track is servable. A relay queries a broadcast's tracks the moment it is
/// announced, so a standby
/// announced any sooner can receive requests for tracks it cannot serve yet.
struct Pending {
	route: moq_net::origin::Route,
	catalog: moq_net::track::Consumer,
}

impl Pending {
	fn new(route: moq_net::origin::Route) -> anyhow::Result<Self> {
		let catalog = route.broadcast().consume().track(hang::Catalog::DEFAULT_NAME)?;
		Ok(Self { route, catalog })
	}

	/// Announce the broadcast if its catalog has gone out, returning whether it has.
	fn announce(&mut self) -> anyhow::Result<bool> {
		if !matches!(
			self.catalog.poll_largest(&kio::Waiter::noop()),
			std::task::Poll::Ready(Some(_))
		) {
			return Ok(false);
		}
		self.route.announce().context("failed to announce broadcast")?;
		Ok(true)
	}
}

impl Publish {
	/// Build a publisher decoding the given container format from stdin into
	/// `broadcast`. After [`announce`](Self::announce), [`run`](Self::run) announces it once the
	/// input has declared its tracks.
	///
	/// Stdin is a live feed with its own zero: the container importers publish its timestamps
	/// so the first frame is live on arrival, verbatim unless `config` pins the catalog clock.
	pub fn new(
		mut broadcast: moq_net::broadcast::Producer,
		format: &PublishFormat,
		config: moq_mux::catalog::Config,
	) -> anyhow::Result<Self> {
		anyhow::ensure!(
			!matches!(format, PublishFormat::Ts { .. }),
			"a TS import publishes through an origin, which republishes it as its source changes"
		);
		let catalog = moq_mux::catalog::Producer::new(&mut broadcast, config)?;
		let decoder = match format {
			PublishFormat::Avc3 => {
				let track = broadcast.unique_track(".avc3", catalog.track_info(hang::catalog::PRIORITY.video))?;
				let import = moq_mux::codec::h264::Import::new(track, catalog.reserve(), Default::default())?;
				let split = Box::new(moq_mux::codec::h264::Split::new());
				PublishDecoder::Avc3 {
					split,
					import: Box::new(import),
				}
			}
			PublishFormat::Fmp4 { cmaf } => {
				let timebase = catalog.timebase();
				let fmp4 = fmp4::Import::new(broadcast.clone(), timebase.reserve()).with_cmaf(*cmaf);
				PublishDecoder::Fmp4(Box::new(fmp4))
			}
			PublishFormat::Ts { .. } => unreachable!("refused above"),
			// The `m2ts` section is in the base catalog: nothing about the multiplex is described
			// beyond the track that carries it.
			PublishFormat::TsPassthrough { pcr_pid } => {
				let timebase = catalog.timebase();
				let mut passthrough = ts::Passthrough::new(broadcast.clone(), timebase.reserve())?;
				if let Some(pid) = pcr_pid {
					passthrough = passthrough.with_pcr_pid(*pid);
				}
				PublishDecoder::TsPassthrough(Box::new(passthrough))
			}
			// FLV's 32-bit milliseconds wrap after 49 days, which its importer does not follow,
			// so it takes no era.
			PublishFormat::Flv => {
				let timebase = catalog.timebase();
				let flv = flv::Import::new(broadcast.clone(), timebase.reserve());
				PublishDecoder::Flv(Box::new(flv))
			}
		};

		Ok(Self {
			source: Source::Stream {
				decoder,
				catalog: PublishCatalog::Media(Box::new(catalog)),
			},
			broadcast: Some(broadcast),
			route: None,
			announce: false,
			report: None,
			linger: moq_mux::Linger::DEFAULT,
			sections: Sections::default(),
		})
	}

	/// Build a publisher decoding `format` from stdin into a new broadcast at `name` on
	/// `origin` under `instance`, retaining its route alongside the decoder's fill handle.
	///
	/// A TS import whose source changes, such as a changeover between two encoders whose clocks
	/// were never locked or whose PATs name another program, finishes the broadcast and republishes
	/// the rest of the input at `name` under a new instance ([`ts::Republish`]). Its first
	/// instance is `instance`.
	pub(super) fn stdin(
		origin: &moq_net::origin::Producer,
		name: &str,
		instance: moq_net::Instance,
		format: &PublishFormat,
		config: moq_mux::catalog::Config,
	) -> anyhow::Result<Self> {
		if let PublishFormat::Ts { program } = *format {
			let mut republish = ts::Republish::new(origin.clone(), moq_net::Path::new(name)?, instance, config)?;
			if let Some(program) = program {
				republish = republish.with_program(program);
			}
			return Ok(Self {
				source: Source::Stream {
					decoder: PublishDecoder::Ts(Box::new(republish)),
					catalog: PublishCatalog::Ts,
				},
				broadcast: None,
				route: None,
				announce: false,
				report: None,
				linger: moq_mux::Linger::DEFAULT,
				sections: Sections::default(),
			});
		}
		let route = origin
			.route(name, moq_net::origin::RouteOptions::hidden().with_instance(instance, 0))
			.context("failed to create broadcast")?;
		let mut result = Self::new(route.broadcast().clone(), format, config)?;
		result.route = Some(route);
		Ok(result)
	}

	/// Build a publisher decoding a multi-program TS from stdin into one broadcast per program,
	/// named after `name` (`event.hang` becomes `event/1.hang`, `event/2.hang`, ...).
	///
	/// The programs aren't known until the PAT arrives, so each broadcast is created then and
	/// announced once its catalog has named a servable track, and [`announce`](Self::announce) has
	/// nothing to do.
	pub fn ts_programs(
		origin: moq_net::origin::Producer,
		name: moq_net::Path,
		config: moq_mux::catalog::Config,
	) -> Self {
		Self {
			source: Source::Stream {
				decoder: PublishDecoder::TsPrograms(Box::new(ts::Programs::new(origin, name, config))),
				catalog: PublishCatalog::Ts,
			},
			broadcast: None,
			route: None,
			announce: false,
			report: None,
			linger: moq_mux::Linger::DEFAULT,
			sections: Sections::default(),
		}
	}

	/// Build a publisher capturing local devices (camera/screen and microphone).
	///
	/// `bandwidth` is the uplink's send estimate, when there is one: the video
	/// encoder follows its share down while the link is congested rather than
	/// overshooting a pipe that can't carry it. Pass `None` to encode at the
	/// configured bitrate regardless.
	///
	/// Audio and video share one allocator, so the video encoder targets what's
	/// left after audio's reservation rather than the whole uplink.
	#[cfg(feature = "capture")]
	pub fn capture(
		route: moq_net::origin::Route,
		args: &CaptureArgs,
		bandwidth: moq_net::bandwidth::Allocator,
		max_age: Option<std::time::Duration>,
	) -> anyhow::Result<Self> {
		let mut broadcast = route.broadcast().clone();
		let config = moq_mux::catalog::Config::default().with_max_age(max_age);
		let catalog = moq_mux::catalog::Producer::new(&mut broadcast, config)?;

		let video = if args.video == Mode::Off {
			None
		} else {
			Some(args.video_capture(bandwidth.clone())?)
		};
		let audio = (args.audio != Mode::Off).then(|| args.audio_capture(bandwidth));
		anyhow::ensure!(video.is_some() || audio.is_some(), "nothing to capture");

		Ok(Self {
			source: Source::Capture {
				broadcast: broadcast.clone(),
				gate: catalog.reserve(),
				catalog,
				video,
				audio,
			},
			broadcast: Some(broadcast),
			route: Some(route),
			announce: false,
			report: None,
			linger: moq_mux::Linger::DEFAULT,
			sections: Sections::default(),
		})
	}

	/// Name a media report track in the catalog's `stats` section and write it beside the
	/// decode loop: what each track has written, and the transport of `connection` when there
	/// is one.
	///
	/// Call it before the importer declares its tracks, so the section rides the first catalog.
	pub fn with_report(mut self, connection: Option<moq_tokio::connection::Monitor>) -> anyhow::Result<Self> {
		let report = match &self.source {
			Source::Stream { decoder, catalog } => match (decoder, catalog, self.broadcast.as_mut()) {
				(_, PublishCatalog::Media(catalog), Some(broadcast)) => {
					crate::report::Stats::new(broadcast, catalog, connection)?
				}
				(PublishDecoder::Ts(republish), _, _) => {
					self.sections.report = Some(connection.clone());
					report(republish, connection)?
				}
				_ => anyhow::bail!("`import ts --program all` builds a catalog per program"),
			},
			#[cfg(feature = "capture")]
			Source::Capture { broadcast, catalog, .. } => {
				crate::report::Stats::new(&mut broadcast.clone(), catalog, connection)?
			}
		};
		self.report = Some(report);
		Ok(self)
	}

	/// Solicit viewer feedback under `echo` in the catalog.
	///
	/// Call it before the importer declares its tracks, so the section rides the first catalog.
	pub fn with_echo(mut self, echo: hang::catalog::Echo) -> anyhow::Result<Self> {
		match &mut self.source {
			Source::Stream { decoder, catalog } => match (decoder, catalog) {
				(_, PublishCatalog::Media(catalog)) => catalog.mutate(|catalog| catalog.echo = Some(echo))?,
				(PublishDecoder::Ts(republish), _) => {
					self.sections.echo = Some(echo.clone());
					republish
						.catalog()
						.clone()
						.mutate(|catalog| catalog.echo = Some(echo))?;
				}
				_ => anyhow::bail!("`import ts --program all` builds a catalog per program"),
			},
			#[cfg(feature = "capture")]
			Source::Capture { catalog, .. } => catalog.mutate(|catalog| catalog.echo = Some(echo))?,
		}
		Ok(self)
	}

	/// Keep a stdin import's broadcast up for `linger` after EOF, so a subscriber that read the
	/// final catalog late still gets the tail. Zero closes it at EOF.
	pub fn with_linger(mut self, linger: std::time::Duration) -> Self {
		self.linger = linger;
		self
	}

	/// Advertise the broadcast's path.
	///
	/// A capture announces now. A stdin import announces from [`run`](Self::run), once the input
	/// has declared its tracks: see [`Pending`].
	pub fn announce(&mut self) -> anyhow::Result<()> {
		match (&self.source, &self.route) {
			(_, None) => {}
			(Source::Stream { .. }, Some(_)) => self.announce = true,
			#[cfg(feature = "capture")]
			(Source::Capture { .. }, Some(route)) => route.announce().context("failed to announce broadcast")?,
		}
		Ok(())
	}

	/// Drive the source until stdin EOF (or the capture devices stop).
	pub async fn run(self) -> anyhow::Result<()> {
		match self.source {
			Source::Stream { decoder, catalog } => {
				let pending = match (&self.route, self.announce) {
					(Some(route), true) => Some(Pending::new(route.clone())?),
					_ => None,
				};
				let publication = Publication {
					control: self.route,
					broadcast: self.broadcast,
					pending,
					report: self.report,
					linger: self.linger,
					sections: self.sections,
				};
				decode(decoder, catalog, tokio::io::stdin(), publication).await
			}
			#[cfg(feature = "capture")]
			Source::Capture {
				broadcast,
				catalog,
				video,
				audio,
				gate,
			} => {
				drop(gate);
				let mut report = self.report;
				let has_video = video.is_some();
				// Each enabled medium publishes its own track onto the shared
				// broadcast + catalog. Both stamp frames on the catalog's
				// advertised clock, so HLS/DASH wall times match the mapping on
				// the wire. In demand mode, video captures only while
				// subscribed and audio retains raw PCM until then. Both run on this task rather
				// than a spawn: on macOS the audio future holds ObjC handles
				// across an await, so it is `!Send`.
				let video_fut = {
					let broadcast = broadcast.clone();
					let catalog = catalog.clone();
					async move {
						match video {
							Some(options) => {
								let (_publication, driver) =
									moq_video::encode::publish_capture(broadcast, catalog, options)?;
								driver.run().await.map_err(anyhow::Error::from)
							}
							None => Ok(()),
						}
					}
				};
				let audio_fut = async move {
					match audio {
						Some(audio) => publish_audio(broadcast, catalog, audio).await,
						None => Ok(Ok(())),
					}
				};

				// A video failure ends the publish. An audio failure that outlasted its reopens
				// ends only the audio track: the video keeps publishing, and the command reports
				// the audio failure once the video ends.
				let capture = async {
					let mut video_fut = std::pin::pin!(video_fut);
					let mut audio_fut = std::pin::pin!(audio_fut);
					tokio::select! {
						result = &mut video_fut => {
							result?;
							audio_fut.await?
						}
						result = &mut audio_fut => {
							let ended = result?;
							if let Err(err) = &ended && has_video {
								tracing::error!(error = %format_args!("{err:#}"), "audio capture ended; video keeps publishing");
							}
							video_fut.await.and(ended)
						}
					}
				};
				// Written beside the encoders, so a capture waiting on its first viewer still reports.
				let result = match report.as_mut() {
					Some(report) => tokio::select! {
						result = capture => result,
						never = report.run() => match never {},
					},
					None => capture.await,
				};
				match (&result, report) {
					(Ok(()), Some(mut report)) => report.finish(),
					(Err(err), Some(report)) => report.abort(moq_net::Error::Transport(err.to_string())),
					(_, None) => {}
				}
				result
			}
		}
	}
}

/// Publish the captured audio, reopening the device and its track when the capture fails
/// mid-session, as a USB microphone re-enumerating or a sample-rate change does.
///
/// moq-audio never reopens a failed device, so this loop does. Each reopen publishes a new track,
/// since the failed one ends with its error. Returns once the track closes, or with the last
/// failure once failures outlast [`Reopen::BUDGET`].
///
/// A first open or publication that fails is a setup problem, such as a wrong device id or a
/// denied permission, so it fails the whole publish at once: the outer error. The inner one ends
/// only the audio.
#[cfg(feature = "capture")]
async fn publish_audio(
	broadcast: moq_net::broadcast::Producer,
	catalog: moq_mux::catalog::Producer,
	(config, encode, retain): (moq_audio::capture::Config, moq_audio::encode::Options, bool),
) -> anyhow::Result<anyhow::Result<()>> {
	// A failed track keeps serving its retained audio until it is served or expires, up to
	// `--max-age`, without holding up the reopen or the report.
	let mut draining = tokio::task::JoinSet::new();
	let mut reopen = Reopen::default();
	let mut published = false;
	let result = loop {
		let mut live = None;
		let attempt = async {
			let capture = moq_audio::capture::Producer::open(config.clone()).await?;
			live = Some(kio::clock::Instant::now());
			let mut options = moq_audio::encode::Capture::new(capture.consume());
			options.encode = encode.clone();
			options.retain = retain;
			let (_publication, driver) =
				moq_audio::encode::publish_capture(broadcast.clone(), catalog.clone(), options)?;
			published = true;
			let mut publish = Box::pin(driver.run());
			tokio::select! {
				result = capture.run() => {
					draining.spawn(publish);
					result
				}
				result = &mut publish => result,
			}
		};
		let err = match attempt.await {
			Ok(()) => break Ok(()),
			Err(err) => anyhow::Error::from(err),
		};
		if !published {
			return Err(err);
		}
		while draining.try_join_next().is_some() {}
		match reopen.failed(live, kio::clock::Instant::now()) {
			Some(wait) => {
				tracing::warn!(error = %format_args!("{err:#}"), ?wait, "audio capture failed; reopening");
				kio::clock::sleep(wait).await;
			}
			None => break Err(err.context(format!("audio capture kept failing for {:?}", Reopen::BUDGET))),
		}
	};
	// Report at once: the failed tracks keep draining in the background while the video runs,
	// each ending with its error, and are dropped when the command exits.
	draining.detach_all();
	Ok(result)
}

/// When to reopen a failed audio capture, and when to give up on it.
#[cfg(feature = "capture")]
struct Reopen {
	/// The wait before the next reopen.
	delay: std::time::Duration,
	/// When the current run of failures began, with no healthy capture since.
	failing: Option<kio::clock::Instant>,
}

#[cfg(feature = "capture")]
impl Default for Reopen {
	fn default() -> Self {
		Self {
			delay: Self::MIN,
			failing: None,
		}
	}
}

#[cfg(feature = "capture")]
impl Reopen {
	/// The first wait. It covers a USB device re-enumerating.
	const MIN: std::time::Duration = std::time::Duration::from_millis(500);
	/// The longest wait, so a returning device is picked up within seconds.
	const MAX: std::time::Duration = std::time::Duration::from_secs(4);
	/// A capture that ran this long recovered, so its failure starts a fresh run of retries.
	const HEALTHY: std::time::Duration = std::time::Duration::from_secs(1);
	/// How long failures may persist before the audio track ends.
	const BUDGET: std::time::Duration = std::time::Duration::from_secs(10);

	/// Record a failure at `now` of a capture live since `live` (`None` when it never opened),
	/// returning the wait before the next reopen, or `None` to give up.
	fn failed(&mut self, live: Option<kio::clock::Instant>, now: kio::clock::Instant) -> Option<std::time::Duration> {
		if live.is_some_and(|live| now.saturating_duration_since(live) >= Self::HEALTHY) {
			*self = Self::default();
		}
		let failing = *self.failing.get_or_insert(now);
		if now.saturating_duration_since(failing) >= Self::BUDGET {
			return None;
		}
		let wait = self.delay;
		self.delay = (self.delay * 2).min(Self::MAX);
		Some(wait)
	}
}

/// What a stdin import does around its media: announce, report, and retain the end.
#[derive(Default)]
struct Publication {
	/// The route, held while the broadcast is up so its path stays announced.
	control: Option<moq_net::origin::Route>,
	/// The broadcast to keep up after EOF, or `None` when the decoder owns its broadcasts.
	broadcast: Option<moq_net::broadcast::Producer>,
	/// The announcement held until the catalog first goes out.
	pending: Option<Pending>,
	/// `--report`, written beside the read loop, so a stalled input keeps reporting, and ended
	/// with its final counts when the media tracks are.
	report: Option<crate::report::Stats>,
	/// How long the finished broadcasts stay up after EOF.
	linger: std::time::Duration,
	/// What a TS import sets on each instance it republishes.
	sections: Sections,
}

/// The catalog sections a stdin TS import sets again on each instance it republishes.
#[derive(Clone, Default)]
struct Sections {
	/// The connection `--report` measures, when the import reports.
	report: Option<Option<moq_tokio::connection::Monitor>>,
	echo: Option<hang::catalog::Echo>,
}

impl Sections {
	/// Set these sections on the instance `republish` just opened, before it declares a track so
	/// they ride its first catalog, returning its report.
	fn apply(&self, republish: &ts::Republish) -> anyhow::Result<Option<crate::report::Stats>> {
		if let Some(echo) = &self.echo {
			republish
				.catalog()
				.clone()
				.mutate(|catalog| catalog.echo = Some(echo.clone()))?;
		}
		self.report
			.as_ref()
			.map(|connection| report(republish, connection.clone()))
			.transpose()
	}
}

/// A `--report` track on the current instance of `republish`.
fn report(
	republish: &ts::Republish,
	connection: Option<moq_tokio::connection::Monitor>,
) -> anyhow::Result<crate::report::Stats> {
	crate::report::Stats::new(&mut republish.broadcast().clone(), republish.catalog(), connection)
}

/// One publisher instance of a stdin import: the decoder filling its broadcast, and what
/// announces and reports it.
struct Instance {
	decoder: PublishDecoder,
	catalog: PublishCatalog,
	control: Option<moq_net::origin::Route>,
	broadcast: Option<moq_net::broadcast::Producer>,
	pending: Option<Pending>,
	report: Option<crate::report::Stats>,
}

/// The input read so far, and the import's counters logged as they move.
struct Reader<R> {
	input: R,
	buffer: bytes::BytesMut,
	// Counters reported so far, so only the change is logged. A live feed is
	// diagnosed by the rate at which these climb, and stdin may never end, so
	// they have to surface as they accumulate rather than at exit.
	log: ts::stats::Log,
	sampled: kio::clock::Instant,
}

/// Decode `input` into the broadcast until EOF.
///
/// At EOF the media tracks finish, then the catalog does, while it still lists them: the
/// renditions retire from the catalog as the decoder drops, which a subscriber would read
/// as removed tracks, and a catalog dropped unfinished reads as a publisher that vanished.
/// The finished broadcasts then stay servable for the linger, so a subscriber that saw the
/// final catalog only at EOF can still request them.
///
/// A change of source ([`ts::Republished`]) ends a TS import's instance there, and the rest of the
/// input publishes a new instance on the same path.
async fn decode(
	decoder: PublishDecoder,
	catalog: PublishCatalog,
	input: impl tokio::io::AsyncRead + Unpin,
	publication: Publication,
) -> anyhow::Result<()> {
	let Publication {
		control,
		broadcast,
		pending,
		report,
		linger,
		sections,
	} = publication;
	let mut instance = Instance {
		decoder,
		catalog,
		control,
		broadcast,
		pending,
		report,
	};
	let mut reader = Reader {
		input,
		// An empty BytesMut grows to only 64 bytes and clearing it keeps that capacity.
		// Stdin dispatches every read to a blocking thread, so those tiny reads can
		// backpressure a live feed. Reserve a full IO chunk; read_buf still returns
		// whatever is available without waiting to fill it.
		buffer: bytes::BytesMut::with_capacity(64 * 1024),
		log: ts::stats::Log::default(),
		sampled: kio::clock::now(),
	};

	let result = loop {
		let republished = match instance.read(&mut reader).await {
			Ok(Some(republished)) => republished,
			Ok(None) => break Ok(()),
			Err(err) => break Err(err),
		};
		if let Err(err) = instance.republished(&sections, republished, &mut reader.log) {
			break Err(err);
		}
	};
	instance.end(result, &mut reader.log, linger).await
}

impl Instance {
	/// Read and decode until EOF, returning `None`, or until a change of source ends the
	/// instance.
	async fn read<R: tokio::io::AsyncRead + Unpin>(
		&mut self,
		reader: &mut Reader<R>,
	) -> anyhow::Result<Option<ts::Republished>> {
		let Self {
			decoder,
			pending,
			report,
			..
		} = self;
		let sections = report.as_ref().map(crate::report::Stats::sections);
		// Run the read/decode loop so an error surfaces to the caller rather than
		// dropping the decoder (and its tracks) with a bare Error::Dropped.
		let read = async {
			loop {
				reader.buffer.clear();
				let n = tokio::io::AsyncReadExt::read_buf(&mut reader.input, &mut reader.buffer).await?;
				if n == 0 {
					return Ok(None); // EOF
				}
				if let Some(republished) = feed(decoder, pending, &reader.buffer)? {
					return Ok(Some(republished));
				}

				if reader.sampled.elapsed() >= ts::stats::Log::INTERVAL {
					reader.sampled = kio::clock::now();
					if let Some(stats) = decoder.stats() {
						if let Some(sections) = &sections {
							sections.set(|report| report.mpegts = Some(stats.clone()));
						}
						reader.log.sample(stats);
					}
				}
			}
		};
		match report.as_mut() {
			Some(report) => tokio::select! {
				result = read => result,
				never = report.run() => match never {},
			},
			None => read.await,
		}
	}

	/// The source changed, and the importer republished: end this instance's report, and set the
	/// sections on the new instance before it reads the rest of the input.
	fn republished(
		&mut self,
		sections: &Sections,
		republished: ts::Republished,
		log: &mut ts::stats::Log,
	) -> anyhow::Result<()> {
		let mut republished = republished;
		loop {
			let PublishDecoder::Ts(republish) = &self.decoder else {
				unreachable!("only a TS import republishes");
			};
			tracing::warn!(step = %republished.step, instance = %republish.instance(), "the source changed; republishing");
			report_mpegts(&self.report, &republished.stats);
			log.finish(&republished.stats);
			if let Some(report) = &mut self.report {
				report.finish();
			}
			self.report = sections.apply(republish)?;
			*log = ts::stats::Log::default();
			match feed(&mut self.decoder, &mut self.pending, &[])? {
				Some(next) => republished = next,
				None => return Ok(()),
			}
		}
	}

	/// Finish on a clean EOF and serve the finished broadcasts for the linger, or abort with
	/// the real cause of `result`, so subscribers see it instead of a bare Error::Dropped.
	async fn end(
		self,
		result: anyhow::Result<()>,
		log: &mut ts::stats::Log,
		linger: std::time::Duration,
	) -> anyhow::Result<()> {
		let Self {
			decoder,
			mut catalog,
			control,
			broadcast,
			mut pending,
			mut report,
		} = self;
		// Flush on a clean EOF; on any error (read, decode, or the flush
		// itself) abort with the real cause.
		let finished = match result {
			Ok(()) => decoder.finish(),
			Err(err) => {
				if let Some(stats) = decoder.stats() {
					report_mpegts(&report, &stats);
					log.finish(&stats);
				}
				decoder.abort(moq_net::Error::Transport(err.to_string()));
				return Err(fail(catalog, report, err));
			}
		};
		let decoder = match finished {
			Ok(decoder) => decoder,
			Err(err) => return Err(fail(catalog, report, err)),
		};
		// The drain at end of input can publish a frame nothing vouched for, so the
		// final snapshot is only complete after `finish`.
		if let Some(stats) = decoder.stats() {
			report_mpegts(&report, &stats);
			log.finish(&stats);
		}
		// The drain can release the first catalog. Without one the input declared no track, and
		// announcing would only advertise an empty broadcast.
		if let Some(announce) = &mut pending {
			let declared = announce.announce().and_then(|declared| {
				anyhow::ensure!(declared, "input ended before it declared a track");
				Ok(())
			});
			if let Err(err) = declared {
				decoder.abort(moq_net::Error::Transport(err.to_string()));
				return Err(fail(catalog, report, err));
			}
		}
		drop(pending);

		// Finished before the broadcast is, so it lingers with the media.
		if let Some(report) = &mut report {
			report.finish();
		}
		catalog.finish()?;
		let mut broadcasts = decoder.broadcasts();
		broadcasts.extend(broadcast);
		tracing::info!(?linger, "input ended; serving the finished broadcast");
		// The decoder and catalog hold the finished tracks, so they go only once it closes.
		moq_mux::Linger::new(broadcasts, (decoder, catalog, report, control), linger)
			.wait()
			.await;
		Ok(())
	}
}

/// Set the report's `mpegts` section, when the import reports.
fn report_mpegts(report: &Option<crate::report::Stats>, snapshot: &ts::stats::Snapshot) {
	if let Some(report) = report {
		report.sections().set(|report| report.mpegts = Some(snapshot.clone()));
	}
}

/// Abort the catalog and report with `err` so subscribers see the real cause, and return it.
fn fail(catalog: PublishCatalog, report: Option<crate::report::Stats>, err: anyhow::Error) -> anyhow::Error {
	let reason = moq_net::Error::Transport(err.to_string());
	if let Some(report) = report {
		report.abort(reason.clone());
	}
	catalog.abort(reason);
	err
}

/// Decode `chunk` into `decoder`, announcing `pending` once its catalog goes out, or return the
/// change of source that ended the instance.
fn feed(
	decoder: &mut PublishDecoder,
	pending: &mut Option<Pending>,
	chunk: &[u8],
) -> anyhow::Result<Option<ts::Republished>> {
	if let Some(republished) = decoder.decode_chunk(chunk)? {
		return Ok(Some(republished));
	}
	if let Some(announce) = pending
		&& announce.announce()?
	{
		*pending = None;
	}
	Ok(None)
}

#[cfg(feature = "capture")]
impl CaptureArgs {
	pub(crate) fn deprecated(&self) -> moq_tokio::cli::Deprecated {
		let mut found = moq_tokio::cli::Deprecated::default();
		if self.no_video {
			found.flag("--no-video", None, "--video off");
		}
		if self.no_audio {
			found.flag("--no-audio", None, "--audio off");
		}
		found
	}

	/// Refuse disabled sources and an invocation with nothing to capture before publishing.
	pub(crate) fn validate(&self) -> anyhow::Result<()> {
		anyhow::ensure!(
			self.video != Mode::Off || self.audio != Mode::Off,
			"--video off and --audio off leave nothing to capture"
		);
		anyhow::ensure!(
			self.video != Mode::Off
				|| (self.camera.is_none() && self.display.is_none() && self.window.is_none() && self.app.is_none()),
			"--video off conflicts with --camera, --display, --window, and --app"
		);
		anyhow::ensure!(
			self.audio != Mode::Off || (self.microphone.is_none() && !self.system_audio),
			"--audio off conflicts with --microphone and --system-audio"
		);
		Ok(())
	}

	/// The video source named by the flags, defaulting to the default camera.
	/// The `video-source` arg group makes these mutually exclusive, so the order
	/// here only decides the default.
	fn video_source(&self) -> anyhow::Result<moq_video::capture::Source> {
		use moq_video::capture::Source;

		Ok(if let Some(id) = &self.window {
			match id {
				Some(id) => Source::Window(id.clone()),
				None => {
					#[cfg(all(target_os = "linux", feature = "pipewire"))]
					{
						use moq_video::capture::portal::{Kind, Selection};
						Source::Portal(Selection::new(Kind::Window))
					}
					#[cfg(not(all(target_os = "linux", feature = "pipewire")))]
					anyhow::bail!(
						"bare --window requires Linux with the pipewire feature; use --window <ID> from `moq devices`"
					);
				}
			}
		} else if let Some(id) = &self.app {
			Source::App(id.clone())
		} else if let Some(id) = &self.display {
			match id {
				Some(id) => Source::Display(moq_video::capture::DisplaySelection::new(id.clone())),
				// Wayland has no main display to name, so the system picker chooses the screen.
				#[cfg(all(target_os = "linux", feature = "pipewire"))]
				None if std::env::var_os("WAYLAND_DISPLAY").is_some() => {
					use moq_video::capture::portal::{Kind, Selection};
					Source::Portal(Selection::new(Kind::Screen))
				}
				None => Source::Display(moq_video::capture::DisplaySelection::default()),
			}
		} else {
			Source::Camera(self.camera.clone().flatten())
		})
	}

	fn video_config(&self) -> anyhow::Result<moq_video::capture::Config> {
		let mut config = moq_video::capture::Config::default();
		config.source = self.video_source()?;
		config.width = self.width;
		config.height = self.height;
		config.framerate = self
			.fps
			.map(|fps| moq_video::FrameRate::new(fps, 1))
			.transpose()
			.map_err(anyhow::Error::from)?;
		config.cursor = match self.no_cursor {
			true => moq_video::capture::Cursor::Hidden,
			false => moq_video::capture::Cursor::Drawn,
		};
		Ok(config)
	}

	fn video_capture(&self, bandwidth: moq_net::bandwidth::Allocator) -> anyhow::Result<moq_video::encode::Capture> {
		let mut options = moq_video::encode::Capture::default();
		options.capture = self.video_config()?;
		options.bitrate = self.bitrate.map(moq_net::bandwidth::Rate::from_bps);
		options.codec = self.codec.into();
		options.kind = if self.software {
			moq_video::encode::Kind::Software
		} else if self.hardware {
			moq_video::encode::Kind::Hardware
		} else {
			moq_video::encode::Kind::Auto
		};
		options.bandwidth = bandwidth;
		options.on_demand = self.video == Mode::Demand;
		options.device = crate::device(&self.vaapi_device, &self.v4l2_encoder);
		Ok(options)
	}

	/// The audio source named by the flags, defaulting to the default
	/// microphone. The `audio-source` arg group makes these mutually exclusive.
	fn audio_source(&self) -> moq_audio::capture::Source {
		use moq_audio::capture::Source;

		if self.system_audio {
			Source::System
		} else {
			Source::Microphone(self.microphone.clone().flatten())
		}
	}

	fn audio_config(&self) -> moq_audio::capture::Config {
		let mut config = moq_audio::capture::Config::default();
		config.source = self.audio_source();
		config
	}

	/// The audio encode settings. `track` is left unset so the name derives
	/// from the codec, the way the video side names its track; consumers find
	/// it through the catalog either way. The source sets the kind, as
	/// `@moq/publish`'s microphone and screen sources do.
	fn audio_encode(&self, bandwidth: moq_net::bandwidth::Allocator) -> moq_audio::encode::Options {
		use moq_audio::encode::Kind;

		let mut options = moq_audio::encode::Options::default();
		options.config.kind = Some(match self.system_audio {
			true => Kind::Music,
			false => Kind::Voice,
		});
		options.config.bitrate = self
			.audio_bitrate
			.map(|bps| moq_net::bandwidth::Rate::from_bps(bps.into()));
		options.bandwidth = bandwidth;
		options
	}

	/// The audio counterpart to [`video_capture`](Self::video_capture).
	fn audio_capture(
		&self,
		bandwidth: moq_net::bandwidth::Allocator,
	) -> (moq_audio::capture::Config, moq_audio::encode::Options, bool) {
		(
			self.audio_config(),
			self.audio_encode(bandwidth),
			self.audio == Mode::Demand,
		)
	}
}

#[cfg(test)]
pub(crate) mod tests {
	use std::time::Duration;

	use bytes::BytesMut;
	use moq_mux::catalog::CatalogFormat;
	use moq_mux::catalog::hang::{Catalog, Container};
	use moq_mux::container::ts::{self as tscat, Export, Import};
	use moq_mux::container::{Consumer, Frame, Producer};
	use moq_net::Timestamp;

	use super::*;

	/// Each wait in a run of reopens that never open, from the first failure until giving up.
	#[cfg(feature = "capture")]
	fn reopen_waits(reopen: &mut Reopen, mut now: kio::clock::Instant) -> Vec<Duration> {
		let mut waits = Vec::new();
		while let Some(wait) = reopen.failed(None, now) {
			waits.push(wait);
			now = now.saturating_add(wait);
		}
		waits
	}

	#[cfg(feature = "capture")]
	#[test]
	fn a_failing_audio_capture_backs_off_then_gives_up() {
		let ms = Duration::from_millis;
		let waits = reopen_waits(&mut Reopen::default(), kio::clock::Instant::ZERO);
		assert_eq!(waits, [ms(500), ms(1000), ms(2000), ms(4000), ms(4000)]);
		assert!(waits.iter().sum::<Duration>() >= Reopen::BUDGET);
	}

	#[cfg(feature = "capture")]
	#[test]
	fn a_healthy_audio_capture_restarts_its_reopens() {
		let start = kio::clock::Instant::ZERO;
		let mut reopen = Reopen::default();
		reopen_waits(&mut reopen, start);

		// A capture that ran past healthy recovered, however long the failures before it.
		let live = start.saturating_add(Duration::from_secs(60));
		let failed = live.saturating_add(Reopen::HEALTHY);
		assert_eq!(reopen.failed(Some(live), failed), Some(Reopen::MIN));
		assert_eq!(
			reopen.failed(None, failed.saturating_add(Reopen::MIN)),
			Some(Reopen::MIN * 2)
		);
	}

	#[cfg(feature = "capture")]
	#[test]
	fn a_flapping_audio_capture_still_gives_up() {
		// Opens that fail sooner than healthy are no recovery, so the budget still runs out.
		let mut reopen = Reopen::default();
		let mut now = kio::clock::Instant::ZERO;
		let mut attempts = 0;
		while let Some(wait) = reopen.failed(Some(now), now.saturating_add(Reopen::HEALTHY / 2)) {
			now = now.saturating_add(Reopen::HEALTHY / 2 + wait);
			attempts += 1;
			assert!(attempts < 100, "a flapping capture never gave up");
		}
		assert_eq!(attempts, 5);
	}

	#[cfg(feature = "capture")]
	fn capture_args(flags: &[&str]) -> CaptureArgs {
		let mut argv = vec!["moq", "--connect", "http://localhost", "import", "capture"];
		argv.extend_from_slice(flags);
		let cli = crate::args::Invocation::try_parse_from(argv).unwrap();
		let crate::args::Command::Import(import) = cli.stages.into_iter().next().unwrap() else {
			panic!("expected import");
		};
		let crate::args::ImportSource::Capture(args) = import.source else {
			panic!("expected capture");
		};
		args
	}

	#[cfg(feature = "capture")]
	#[test]
	fn capture_window_picker_accepts_following_flags() {
		let args = capture_args(&["--window", "--audio", "off"]);
		assert_eq!(args.audio, Mode::Off);
		assert_eq!(args.window, Some(None));
		assert_eq!(capture_args(&["--window"]).window, Some(None));
	}

	#[cfg(feature = "capture")]
	#[test]
	fn capture_modes_are_independent() {
		use moq_net::bandwidth::Allocator;

		let args = capture_args(&[]);
		assert!(!args.video_capture(Allocator::unlimited()).unwrap().on_demand);
		assert!(args.audio_capture(Allocator::unlimited()).2);

		for (video, audio) in [("on", "on"), ("on", "demand"), ("demand", "on"), ("demand", "demand")] {
			let args = capture_args(&["--video", video, "--audio", audio]);
			args.validate().unwrap();
			assert_eq!(
				args.video_capture(Allocator::unlimited()).unwrap().on_demand,
				video == "demand"
			);
			assert_eq!(args.audio_capture(Allocator::unlimited()).2, audio == "demand");
		}
	}

	#[cfg(feature = "capture")]
	#[test]
	fn the_audio_source_sets_its_kind() {
		use moq_audio::encode::Kind;
		use moq_net::bandwidth::Allocator;

		for (flags, kind) in [
			(&[][..], Kind::Voice),
			(&["--microphone"][..], Kind::Voice),
			(&["--system-audio"][..], Kind::Music),
		] {
			let options = capture_args(flags).audio_capture(Allocator::unlimited()).1;
			assert_eq!(options.config.kind, Some(kind), "{flags:?}");
		}
	}

	#[cfg(feature = "capture")]
	#[test]
	fn capture_off_refuses_selected_sources_and_empty_publications() {
		for medium in ["--video", "--audio"] {
			capture_args(&[medium, "off"]).validate().unwrap();
		}
		for flags in [
			["--video", "off", "--audio", "off"].as_slice(),
			&["--video", "off", "--camera"],
			&["--display", "--video", "off"],
			&["--window", "123", "--video", "off"],
			&["--video", "off", "--app", "app"],
			&["--audio", "off", "--microphone"],
			&["--system-audio", "--audio", "off"],
		] {
			let argv = [vec!["moq", "import", "capture"], flags.to_vec()].concat();
			assert!(
				crate::args::Invocation::try_parse_from(argv)
					.unwrap()
					.validate()
					.is_err(),
				"{flags:?}"
			);
		}
	}

	#[cfg(feature = "capture")]
	#[test]
	fn capture_window_id_preserves_native_selection() {
		for id in ["39193", "x11:0x400001"] {
			let config = capture_args(&["--window", id, "--audio", "off"])
				.video_config()
				.unwrap();
			assert!(matches!(config.source, moq_video::capture::Source::Window(value) if value == id));
		}
	}

	#[cfg(feature = "capture")]
	#[test]
	fn capture_window_picker_conflicts_with_other_sources() {
		for flag in ["--camera", "--display", "--app"] {
			assert!(
				crate::args::Invocation::try_parse_from(["moq", "import", "capture", "--window", flag, "other",])
					.is_err()
			);
		}
	}

	#[cfg(all(feature = "capture", target_os = "linux", feature = "pipewire"))]
	#[test]
	fn capture_window_picker_uses_portal() {
		let config = capture_args(&["--window", "--audio", "off"]).video_config().unwrap();
		assert!(matches!(config.source, moq_video::capture::Source::Portal(_)));
	}

	#[cfg(all(feature = "capture", not(all(target_os = "linux", feature = "pipewire"))))]
	#[test]
	fn capture_window_picker_refuses_unsupported_builds() {
		let error = capture_args(&["--window", "--audio", "off"])
			.video_config()
			.unwrap_err();
		assert!(
			error
				.to_string()
				.contains("bare --window requires Linux with the pipewire feature")
		);
	}

	/// Real H.264 + AAC TS, reused to give the manufactured input a video clock
	/// (section-framed verbatim export requires one) and decodable media tracks.
	const BBB: &[u8] = include_bytes!("../../moq-mux/src/container/ts/test_data/bbb.ts");

	/// A libklvanc public-sample SCTE-35 splice_info_section (table_id 0xFC), carried
	/// on a section-framed PID. Same bytes the moq-mux export round-trip test uses.
	const CUE: &[u8] = &[
		0xfc, 0x30, 0x1b, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xff, 0xf0, 0x0a, 0x05, 0x00, 0x00, 0x2b, 0xb4,
		0x7f, 0xdf, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0xad, 0x25, 0xe8, 0x39,
	];

	/// A whole PES of an undecoded stream (private_stream_1, no PTS), carried on its own PID.
	const PES: &[u8] = &[
		0x00, 0x00, 0x01, 0xBD, 0x00, 0x09, 0x84, 0x00, 0x00, 0xDE, 0xAD, 0xBE, 0xEF, 0x01, 0x02,
	];

	const SECTION_PID: u16 = 0x102;
	const PES_PID: u16 = 0x104;

	async fn open_export(
		source: moq_mux::Source,
		format: CatalogFormat,
		delay: Duration,
	) -> moq_mux::Result<Export<moq_mux::catalog::Consumer<tscat::Ext>>> {
		let catalog = source.catalog::<tscat::Ext>(format).await?;
		Ok(Export::new(source, catalog, delay))
	}

	/// Drain an exporter, concatenating every frame's payload until output stops. The
	/// producers stay alive (retained tracks), so the stream never hard-ends; pull until a
	/// `next()` blocks, surfaced here as a timeout once the buffered frames are gone.
	async fn drain(mut exporter: Export<moq_mux::catalog::Consumer<tscat::Ext>>) -> Vec<u8> {
		let mut out = Vec::new();
		// Past the export's release delay and the send-ahead window behind it, so the first
		// frame goes out before it ends.
		let wait = RECORDING_MAX_AGE * 3 + Duration::from_secs(1);
		while let Ok(res) = kio::clock::timeout(wait, exporter.next()).await {
			match res.expect("exporter error") {
				Some(frame) => out.extend_from_slice(&frame.payload),
				None => break,
			}
		}
		out
	}

	/// Manufacture a TS feed carrying real video/audio plus one section-framed
	/// verbatim stream (SCTE-35) and one whole-PES stream, by importing
	/// `bbb.ts` into a broadcast that also holds the two ancillary tracks and
	/// re-exporting with the `mpegts` catalog extension.
	/// Let the origin's driver run the fronts that requests and announcements
	/// started: they serve asynchronously, shortly after the call returns.
	async fn settle() {
		for _ in 0..10 {
			tokio::task::yield_now().await;
		}
	}

	async fn manufacture_input() -> Vec<u8> {
		// Create the broadcast on a throwaway origin so the exporter can resolve it by path.
		let origin = moq_tokio::origin::spawn();
		let broadcast = origin.route("cli", moq_net::origin::RouteOptions::default()).unwrap();
		settle().await;
		let config = moq_mux::catalog::Config::default().with_catalog(Catalog::<tscat::Ext>::default());
		let mut catalog = moq_mux::catalog::Producer::new(&mut broadcast.broadcast().clone(), config).unwrap();
		// Reserve before the hand-written tracks below, so their catalog edits are withheld and
		// the import still places the clock, keeping bbb's PTS those tracks are stamped against.
		let reserved = catalog.reserve();

		// Section-framed verbatim stream (SCTE-35, stream_type 0x86).
		let section = broadcast
			.broadcast()
			.unique_track(".scte35", hang::container::track_info(hang::catalog::PRIORITY.text))
			.unwrap();
		let mut section_track = tscat::Track::new(SECTION_PID);
		section_track.verbatim = Some(tscat::Verbatim::new(0x86, tscat::Framing::Section));
		catalog
			.modify()
			.unwrap()
			.ext
			.mpegts
			.tracks
			.insert(section.name().to_string(), section_track);
		let mut section_producer = Producer::new(section, Container::Legacy(moq_mux::container::Kind::Data));
		// The export joins each media track at its newest group, bbb's last at about
		// 1.446 s; stamp the ancillary streams just after it so they clear the keyframe
		// alignment (anything before the first keyframe is dropped on tune-in).
		section_producer
			.write(Frame {
				timestamp: Timestamp::from_millis(1450).unwrap(),
				duration: None,
				payload: bytes::Bytes::from_static(CUE),
				keyframe: true,
			})
			.unwrap();
		section_producer.cut(None).unwrap();
		let _finished = section_producer.finish().unwrap();

		// Whole-PES stream (undecoded private data, stream_type 0x06).
		let pes = broadcast
			.broadcast()
			.unique_track(".data", hang::container::track_info(hang::catalog::PRIORITY.text))
			.unwrap();
		let mut pes_track = tscat::Track::new(PES_PID);
		pes_track.pes_packet = Some(tscat::PesPacket::new(0x06));
		catalog
			.modify()
			.unwrap()
			.ext
			.mpegts
			.tracks
			.insert(pes.name().to_string(), pes_track);
		let mut pes_producer = Producer::new(pes, Container::Legacy(moq_mux::container::Kind::Data));
		pes_producer
			.write(Frame {
				timestamp: Timestamp::from_millis(1450).unwrap(),
				duration: None,
				payload: bytes::Bytes::from_static(PES),
				keyframe: true,
			})
			.unwrap();
		pes_producer.cut(None).unwrap();
		let _finished = pes_producer.finish().unwrap();

		// Add the real video/audio (moves `broadcast` into the importer).
		let mut import = Import::new(broadcast.broadcast().clone(), reserved);
		import.decode(&BytesMut::from(BBB)).unwrap();
		let _finished = import.finish().unwrap();

		// `catalog`, the producers, and `import` stay alive: the exporter subscribes to
		// the retained tracks.
		drain(
			open_export(resolved(&origin, "cli").await, CatalogFormat::Hang, RECORDING_MAX_AGE)
				.await
				.unwrap(),
		)
		.await
	}

	/// A stdin TS import of `demo` on an origin of its own, and a reader of its first instance's
	/// broadcast.
	fn ts_stdin() -> (Publish, moq_net::broadcast::Consumer) {
		let (origin, driver) = moq_net::origin::Producer::new(moq_net::origin::Config::default());
		if tokio::runtime::Handle::try_current().is_ok() {
			tokio::spawn(moq_net::time::run(driver));
		} else {
			// Nothing polls the driver in a sync test, and dropping it would tear the origin down.
			std::mem::forget(driver);
		}
		let publish = Publish::stdin(
			&origin,
			"demo",
			moq_net::Instance::random(),
			&PublishFormat::Ts { program: None },
			Default::default(),
		)
		.unwrap();
		let Source::Stream {
			decoder: PublishDecoder::Ts(republish),
			..
		} = &publish.source
		else {
			panic!("a TS import");
		};
		let consumer = republish.broadcast().consume();
		(publish, consumer)
	}

	/// A source for the broadcast at `path`, resolved through `origin`.
	async fn resolved(origin: &moq_net::origin::Producer, path: &str) -> moq_mux::Source {
		let consumer = origin.consume();
		let broadcast = consumer.request(path, None).await.unwrap();
		moq_mux::Source::new(consumer, broadcast)
	}

	/// The export's delay, long enough to reach the ancillary tracks: these tests publish a
	/// whole feed before exporting it, and the media tracks start at their newest group.
	const RECORDING_MAX_AGE: std::time::Duration = Duration::from_secs(30);
	/// Full CLI round-trip over the hang catalog.
	#[tokio::test(start_paused = true)]
	async fn ts_verbatim_streams_round_trip_through_cli() {
		ts_verbatim_round_trip(CatalogFormat::Hang).await;
	}

	/// The same round-trip over the MSF catalog: the `mpegts` section rides the MSF
	/// track's root, so the export rebuilds the multiplex from either catalog.
	#[tokio::test(start_paused = true)]
	async fn ts_verbatim_streams_round_trip_through_msf() {
		ts_verbatim_round_trip(CatalogFormat::Msf).await;
	}

	/// Full CLI round-trip: a TS feed with undecoded streams goes through `Publish`
	/// (which selects the `mpegts` catalog) and the subscribe-side catalog stream,
	/// and the SCTE-35 section and the whole PES survive with their PIDs and every byte.
	async fn ts_verbatim_round_trip(format: CatalogFormat) {
		// Paused time auto-advances when the exporter parks, so the `drain` timeouts
		// fire instantly instead of waiting on the wall clock.
		let input = manufacture_input().await;

		// Publish side: a TS import uses the `mpegts` catalog, so the verbatim streams land in
		// the broadcast instead of being dropped by the media-only path. The broadcast is created
		// on a throwaway origin so the exporter can resolve it by path, announced once its
		// catalog goes out.
		let origin = moq_tokio::origin::spawn();
		let publish = Publish::stdin(
			&origin,
			"cli",
			moq_net::Instance::random(),
			&PublishFormat::Ts { program: None },
			Default::default(),
		)
		.unwrap();
		#[cfg_attr(
			not(feature = "capture"),
			expect(
				irrefutable_let_patterns,
				reason = "`Source::Capture` exists only with the capture feature"
			)
		)]
		let Source::Stream { mut decoder, .. } = publish.source else {
			panic!("expected a stream source");
		};
		decoder.decode_chunk(&input).unwrap();
		let _decoder = decoder.finish().unwrap();

		// Subscribe side: the same catalog stream `run_ts` selects, re-emitting the
		// ancillary streams verbatim.
		let output = drain(
			open_export(resolved(&origin, "cli").await, format, RECORDING_MAX_AGE)
				.await
				.unwrap(),
		)
		.await;

		// Re-import the round-tripped TS and inspect the recovered `mpegts` section.
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let consumer = broadcast.consume();
		let config = moq_mux::catalog::Config::default().with_catalog(Catalog::<tscat::Ext>::default());
		let catalog = moq_mux::catalog::Producer::new(&mut broadcast, config).unwrap();
		let mut import = Import::new(broadcast, catalog.reserve());
		import.decode(&BytesMut::from(&output[..])).unwrap();
		let _finished = import.finish().unwrap();
		let snapshot = catalog.snapshot();

		let (section_name, section) = snapshot
			.ext
			.mpegts
			.tracks
			.iter()
			.find(|(_, t)| t.verbatim.as_ref().is_some_and(|v| v.stream_type == 0x86))
			.expect("SCTE-35 section survived the round-trip");
		assert_eq!(section.pid, SECTION_PID, "section PID preserved");
		assert_eq!(
			section.verbatim.as_ref().unwrap().framing,
			tscat::Framing::Section,
			"section framing preserved"
		);
		let section_name = section_name.clone();

		let (pes_name, pes) = snapshot
			.ext
			.mpegts
			.tracks
			.iter()
			.find(|(_, t)| t.pes_packet.as_ref().is_some_and(|p| p.stream_type == 0x06))
			.expect("the PES track survived the round-trip");
		assert_eq!(pes.pid, PES_PID, "PES PID preserved");
		let pes_name = pes_name.clone();

		assert_eq!(
			read_frame(&consumer, &section_name).await,
			CUE,
			"SCTE-35 section round-trips byte-for-byte"
		);
		assert_eq!(
			read_frame(&consumer, &pes_name).await,
			PES,
			"the PES round-trips byte-for-byte"
		);
	}

	/// `moq import ts --passthrough` publishes no renditions, only the `m2ts` section naming one
	/// track, and that track reads back as the input from its first group on.
	#[tokio::test(start_paused = true)]
	async fn ts_passthrough_publishes_the_multiplex_on_one_track() {
		const KYRION: &[u8] = include_bytes!("../../moq-mux/src/container/ts/test_data/kyrion_mpeg2av_ac3.ts");
		let broadcast = moq_net::broadcast::Info::new().produce();
		let consumer = broadcast.consume();
		let publish = Publish::new(
			broadcast,
			&PublishFormat::TsPassthrough { pcr_pid: None },
			Default::default(),
		)
		.unwrap();
		#[cfg_attr(
			not(feature = "capture"),
			expect(
				irrefutable_let_patterns,
				reason = "`Source::Capture` exists only with the capture feature"
			)
		)]
		let Source::Stream { mut decoder, .. } = publish.source else {
			panic!("expected a stream source");
		};
		for chunk in KYRION.chunks(64 * 1024) {
			decoder.decode_chunk(chunk).unwrap();
		}
		let _decoder = decoder.finish().unwrap();

		let catalog = hang::catalog::Catalog::<()>::subscribe(&consumer)
			.await
			.unwrap()
			.next()
			.await
			.unwrap()
			.expect("a catalog")
			.value;
		assert!(catalog.video.renditions.is_empty() && catalog.audio.renditions.is_empty());
		let m2ts = catalog.m2ts.expect("the m2ts section");
		assert!(m2ts.random_access);

		let mut track = consumer
			.track(&m2ts.track)
			.unwrap()
			.subscribe(
				moq_net::track::Subscription::default()
					.with_delay(moq_net::track::Delay::default().with_max(RECORDING_MAX_AGE)),
			)
			.await
			.unwrap();
		let mut output = Vec::new();
		while let Some(mut group) = track.recv_group().await.unwrap() {
			while let Some(frame) = group.read_frame().await.unwrap() {
				output.extend_from_slice(&hang::container::Frame::decode(frame.payload).unwrap().payload);
			}
		}
		assert!(!output.is_empty());
		assert!(KYRION.ends_with(&output), "byte-identical from the first group");
	}

	/// The passthrough track crosses a relay byte for byte on the lite and the IETF wire, through
	/// a flagged PCR jump twenty seconds forward. The jump crosses as one marker group, an object
	/// with no bytes, and the timestamps carry it, so a budget shorter than the jump drops the
	/// groups before it.
	#[tokio::test]
	async fn ts_passthrough_crosses_a_relay_through_a_flagged_jump() {
		const KYRION: &[u8] = include_bytes!("../../moq-mux/src/container/ts/test_data/kyrion_mpeg2av_ac3.ts");
		if !test_env::Env::without(&["MOQ_CONNECT", "MOQ_HOP"]).isolate() {
			return;
		}
		// The capture with its clock `seconds` ahead, its first PCR flagged as a discontinuity
		// when `flagged`.
		let shifted = |seconds: u64, flagged: bool| {
			let mut copy = KYRION.to_vec();
			let mut pcrs = copy
				.as_chunks_mut::<188>()
				.0
				.iter_mut()
				.filter(|pkt| pkt[3] & 0x20 != 0 && pkt[4] > 0 && pkt[5] & 0x10 != 0)
				.peekable();
			if flagged {
				pcrs.peek_mut().expect("a PCR")[5] |= 0x80;
			}
			for pkt in pcrs {
				let base = pkt[6..11].iter().fold(0u64, |base, byte| base << 8 | u64::from(*byte)) >> 7;
				let base = ((base + seconds * 90_000) & ((1 << 33) - 1)) << 7 | u64::from(pkt[10] & 0x7f);
				pkt[6..11].copy_from_slice(&base.to_be_bytes()[3..]);
			}
			copy
		};
		// The capture, carried straight on by a copy four seconds ahead so a whole catalog window
		// follows the first group, then a copy twenty seconds past that one behind a flagged
		// discontinuity. Inside the track's max age, so the recording keeps every group.
		let lead = [KYRION, &shifted(4, false)].concat();
		let second = shifted(24, true);
		let input = [&lead[..], &second].concat();

		moq_tokio::crypto::install_default().unwrap();
		for version in ["moq-lite-06", "moq-lite-07-wip", "moq-transport-18"] {
			let fixture = moq_relay::test_relay().await.unwrap();
			let ready = fixture.relay.ready();
			tokio::spawn(fixture.relay.run());
			ready.wait().await.unwrap();
			let net = crate::Net {
				quic: Default::default(),
			};
			let url = fixture.url.to_string();
			let client = || {
				let cli = crate::args::Invocation::try_parse_from([
					"moq",
					"--connect",
					&url,
					"--connect-tls-fingerprint",
					&fixture.fingerprint,
					"--connect-version",
					version,
					"fetch",
					"x",
				])
				.unwrap();
				net.client(cli.moq.client.clone()).unwrap().with_reconnect(false)
			};

			let origin = moq_tokio::origin::spawn();
			let publish = Publish::stdin(
				&origin,
				"pt.hang",
				moq_net::Instance::random(),
				&PublishFormat::TsPassthrough { pcr_pid: None },
				Default::default(),
			)
			.unwrap();
			publish.route.as_ref().unwrap().announce().unwrap();
			#[cfg_attr(
				not(feature = "capture"),
				expect(
					irrefutable_let_patterns,
					reason = "`Source::Capture` exists only with the capture feature"
				)
			)]
			let Source::Stream { mut decoder, .. } = publish.source else {
				panic!("expected a stream source");
			};
			let _publisher = client()
				.with_publisher(origin.consume())
				.connect(fixture.url.clone())
				.established()
				.await
				.unwrap();
			let remote = moq_tokio::origin::spawn();
			let _subscriber = client()
				.with_subscriber(remote.clone())
				.connect(fixture.url.clone())
				.established()
				.await
				.unwrap();

			// The catalog names the track once a whole window has followed the first group, which
			// the lead holds; the jump is in the copy after it.
			const WAIT: Duration = Duration::from_secs(10);
			decoder.decode_chunk(&lead).unwrap();
			let consumer = kio::clock::timeout(WAIT, remote.consume().follow("pt.hang", None).unwrap().available())
				.await
				.unwrap()
				.unwrap()
				.unwrap();
			let mut catalogs = hang::catalog::Catalog::<()>::subscribe(&consumer).await.unwrap();
			let catalog = kio::clock::timeout(WAIT, catalogs.next())
				.await
				.unwrap()
				.unwrap()
				.expect("a catalog")
				.value;
			let name = catalog.m2ts.expect("the m2ts section").track;

			// Subscribe and take the first group, so a read starts behind anything decoded
			// afterwards: an IETF subscription joins the publisher's current group when its
			// SUBSCRIBE arrives, not when it is sent.
			let start = async |subscription: moq_net::track::Subscription| {
				let mut track = consumer.track(&name).unwrap().subscribe(subscription).await.unwrap();
				let first = kio::clock::timeout(WAIT, track.recv_group()).await.unwrap().unwrap();
				(track, first)
			};
			// Each group's start and frames, read until the track ends. A read cut off as
			// stale leaves `None` for that group's frames, and a marker leaves none.
			let read = |(mut track, first): (moq_net::track::Subscriber, Option<moq_net::group::Consumer>)| {
				tokio::spawn(kio::clock::timeout(WAIT, async move {
					let mut groups = Vec::new();
					let mut next = first;
					while let Some(mut group) = next {
						let mut frames = Some(Vec::new());
						loop {
							match group.read_frame().await {
								Ok(Some(frame)) if frame.payload.is_empty() => {}
								Ok(Some(frame)) => frames
									.as_mut()
									.unwrap()
									.push(hang::container::Frame::decode(frame.payload).unwrap()),
								Ok(None) => break,
								// Cut here, or by the relay.
								Err(moq_net::Error::Old | moq_net::Error::Stream(moq_net::StreamError::Old)) => {
									frames = None;
									break;
								}
								Err(err) => panic!("{err}"),
							}
						}
						groups.push((group.start, frames));
						next = track.recv_group().await.unwrap();
					}
					groups.sort_by_key(|(start, _)| *start);
					groups
				}))
			};
			let recording = read(
				start(
					moq_net::track::Subscription::default()
						.with_delay(moq_net::track::Delay::default().with_max(RECORDING_MAX_AGE)),
				)
				.await,
			);
			// All at once: the track ends while the relay is still pulling the older groups.
			decoder.decode_chunk(&second).unwrap();
			let _decoder = decoder.finish().unwrap();

			let recording: Vec<_> = recording
				.await
				.unwrap()
				.unwrap_or_else(|err| panic!("{version}: the recording ends with the track: {err}"))
				.into_iter()
				.map(|(start, frames)| (start, frames.expect("the recording reads every group whole")))
				.collect();
			let output = recording
				.iter()
				.flat_map(|(_, frames)| frames.iter().map(|frame| &frame.payload[..]))
				.collect::<Vec<_>>()
				.concat();
			assert!(output.len() > KYRION.len(), "{version}: both copies crossed");
			assert!(
				input.ends_with(&output),
				"{version}: byte-identical from the first group"
			);
			let media: Vec<_> = recording.iter().filter(|(_, frames)| !frames.is_empty()).collect();
			let first = |frames: &[hang::container::Frame]| frames[0].timestamp.as_micros().unwrap();
			let jump = media
				.windows(2)
				.find(|pair| first(&pair[1].1) > first(&pair[0].1) + 10_000_000)
				.map(|pair| pair[1].0)
				.unwrap_or_else(|| panic!("{version}: no jump crossed"));
			// Groups are keyed by their starts, so a missing one shows in the bytes above rather than
			// in the keys, which only have to rise.
			let starts: Vec<u64> = recording.iter().map(|(start, _)| *start).collect();
			assert!(
				starts.windows(2).all(|pair| pair[0] < pair[1]),
				"{version}: groups in start order: {starts:?}"
			);
			let at = starts.iter().position(|start| *start == jump).unwrap();
			let markers: Vec<u64> = recording
				.iter()
				.filter(|(_, frames)| frames.is_empty())
				.map(|(start, _)| *start)
				.collect();
			assert_eq!(
				markers,
				[starts[at - 1]],
				"{version}: the break marker precedes the jump"
			);

			// Joining once every group is out, with a budget half the jump, reads every group
			// after it past the marker, and none of the groups the jump left more than
			// the budget behind. An IETF subscription starts at the newest group, so only lite
			// has the groups behind it to read.
			if version.starts_with("moq-lite") {
				let late = read(
					start(
						moq_net::track::Subscription::default()
							.with_delay(moq_net::track::Delay::default().with_max(Duration::from_secs(10))),
					)
					.await,
				)
				.await
				.unwrap()
				.expect("the late read ends with the track");
				let whole: Vec<u64> = late
					.iter()
					.filter(|(_, frames)| frames.is_some())
					.map(|(start, _)| *start)
					.collect();
				assert!(
					whole.ends_with(&starts[at..]),
					"{version}: a late subscriber reads every group from the jump on: {whole:?}"
				);
				assert!(
					whole.iter().all(|start| *start >= starts[at - 2]),
					"{version}: only the group the jump follows is in budget before it: {whole:?}"
				);
			}
		}
	}

	/// `moq import ts` publishes the feed's own PTS (1.4s into bbb) and anchors the catalog
	/// clock on it, so the first frame is live on arrival.
	#[tokio::test]
	async fn ts_import_anchors_the_catalog_clock_on_arrival() {
		let (publish, consumer) = ts_stdin();
		#[cfg_attr(
			not(feature = "capture"),
			expect(
				irrefutable_let_patterns,
				reason = "`Source::Capture` exists only with the capture feature"
			)
		)]
		let Source::Stream { mut decoder, .. } = publish.source else {
			panic!("expected a stream source");
		};
		let before = kio::clock::wall();
		decoder.decode_chunk(BBB).unwrap();
		let after = kio::clock::wall();
		let _decoder = decoder.finish().unwrap();

		let catalog = hang::catalog::Catalog::<()>::subscribe(&consumer)
			.await
			.unwrap()
			.next()
			.await
			.unwrap()
			.expect("a catalog")
			.value;
		let clock = catalog.sync.clock.expect("the catalog advertises a clock");
		let (name, config) = catalog.video.renditions.iter().next().expect("a video rendition");
		let track = consumer.track(name).unwrap().subscribe(None).await.unwrap();
		let container = moq_mux::catalog::hang::Container::try_from(config).unwrap();
		let first = next_frame(&mut Consumer::new(track, container)).await.timestamp;
		assert!(
			first.as_micros().unwrap() < Duration::from_secs(5).as_micros(),
			"the feed's own PTS: {first:?}"
		);
		// The PES that anchors the mapping need not be this frame: the mux spaces them apart.
		let skew = Duration::from_secs(2);
		let wall = clock.wall_clock(first).unwrap();
		assert!(
			before - skew <= wall && wall <= after + skew,
			"the first frame is live on arrival"
		);
	}

	/// A solicitation waits for the input's tracks rather than publishing a catalog, which
	/// would announce a stdin import before it can serve anything.
	#[tokio::test]
	async fn echo_rides_the_first_catalog() {
		let (publish, consumer) = ts_stdin();
		let echo = hang::catalog::Echo::new(moq_net::path::Relative::new("viewers").unwrap());
		let publish = publish.with_echo(echo.clone()).unwrap();
		let mut track = consumer.track(Catalog::DEFAULT_NAME).unwrap();
		let mut published = || {
			matches!(
				track.poll_largest(&kio::Waiter::noop()),
				std::task::Poll::Ready(Some(_))
			)
		};
		assert!(!published(), "a catalog went out before the input declared a track");

		#[cfg_attr(
			not(feature = "capture"),
			expect(
				irrefutable_let_patterns,
				reason = "`Source::Capture` exists only with the capture feature"
			)
		)]
		let Source::Stream { mut decoder, .. } = publish.source else {
			panic!("expected a stream source");
		};
		decoder.decode_chunk(BBB).unwrap();
		let _decoder = decoder.finish().unwrap();
		assert!(published(), "the input's tracks release the catalog");
		let catalog = Catalog::<()>::subscribe(&consumer)
			.await
			.unwrap()
			.next()
			.await
			.unwrap()
			.expect("a catalog")
			.value;
		assert_eq!(catalog.echo, Some(echo));
		assert!(!catalog.video.renditions.is_empty(), "beside the tracks");
	}

	/// A ready TS packet is consumed in one read, without splitting it into blocking
	/// stdin handoffs smaller than the packet itself.
	#[tokio::test(start_paused = true)]
	async fn stdin_reads_whole_available_packets() {
		struct Packets<'a> {
			remaining: &'a [u8],
			reads: usize,
		}
		impl tokio::io::AsyncRead for Packets<'_> {
			fn poll_read(
				mut self: std::pin::Pin<&mut Self>,
				_: &mut std::task::Context<'_>,
				buffer: &mut tokio::io::ReadBuf<'_>,
			) -> std::task::Poll<std::io::Result<()>> {
				self.reads += 1;
				let n = self.remaining.len().min(188).min(buffer.remaining());
				buffer.put_slice(&self.remaining[..n]);
				self.remaining = &self.remaining[n..];
				std::task::Poll::Ready(Ok(()))
			}
		}

		let (publish, _) = ts_stdin();
		#[cfg_attr(
			not(feature = "capture"),
			expect(
				irrefutable_let_patterns,
				reason = "`Source::Capture` exists only with the capture feature"
			)
		)]
		let Source::Stream { decoder, catalog } = publish.source else {
			panic!("expected a stream source");
		};
		let mut input = Packets {
			remaining: BBB,
			reads: 0,
		};
		decode(decoder, catalog, &mut input, Publication::default())
			.await
			.unwrap();
		assert_eq!(
			input.reads,
			BBB.len().div_ceil(188) + 1,
			"one read per available packet, then EOF"
		);
	}

	/// Reports file-backed import throughput, including the blocking IO handoffs used by stdin.
	#[tokio::test]
	#[ignore = "benchmark, run in nightly CI"]
	async fn benchmark_stream_import() {
		use std::io::Write;

		const INPUT: &[u8] = include_bytes!("../../moq-mux/src/container/ts/test_data/bbb_cbr.ts");
		let mut null = [0xff; 188];
		null[..4].copy_from_slice(&[0x47, 0x1f, 0xff, 0x10]);
		let padding = null.repeat(INPUT.len() / 188);
		println!("copies,ns_per_byte");
		for copies in [1, 16, 128] {
			let mut file = tempfile::NamedTempFile::new().unwrap();
			file.write_all(INPUT).unwrap();
			for _ in 1..copies {
				file.write_all(&padding).unwrap();
			}
			file.flush().unwrap();
			let input = tokio::fs::File::open(file.path()).await.unwrap();
			let (publish, _) = ts_stdin();
			#[cfg_attr(
				not(feature = "capture"),
				expect(
					irrefutable_let_patterns,
					reason = "`Source::Capture` exists only with the capture feature"
				)
			)]
			let Source::Stream { decoder, catalog } = publish.source else {
				panic!("expected a stream source");
			};
			let start = kio::clock::now();
			decode(decoder, catalog, input, Publication::default()).await.unwrap();
			let elapsed = start.elapsed();
			let bytes = INPUT.len() * copies;
			println!("{copies},{:.3}", elapsed.as_nanos() as f64 / bytes as f64);
		}
	}

	/// At stdin EOF the catalog ends cleanly and still lists the renditions, so a
	/// subscriber reads a finished broadcast rather than its tracks being removed.
	#[tokio::test(start_paused = true)]
	async fn eof_finishes_the_catalog_with_its_renditions() {
		let (publish, consumer) = ts_stdin();
		let mut catalogs = hang::catalog::Catalog::<()>::subscribe(&consumer).await.unwrap();

		#[cfg_attr(
			not(feature = "capture"),
			expect(
				irrefutable_let_patterns,
				reason = "`Source::Capture` exists only with the capture feature"
			)
		)]
		let Source::Stream { decoder, catalog } = publish.source else {
			panic!("expected a stream source");
		};
		decode(decoder, catalog, BBB, Publication::default()).await.unwrap();
		drop(publish.broadcast);

		let mut last = None;
		loop {
			let next = kio::clock::timeout(Duration::from_secs(1), catalogs.next())
				.await
				.expect("the catalog track ends");
			match next.expect("the catalog ends cleanly") {
				Some(catalog) => last = Some(catalog.value),
				None => break,
			}
		}
		let last = last.expect("a catalog");
		assert_eq!(last.video.renditions.len(), 1, "the video rendition is still listed");
		assert_eq!(last.audio.renditions.len(), 1, "the audio rendition is still listed");
	}

	/// After EOF the finished tracks stay servable for the linger, so a subscriber that reads
	/// the final catalog only then still gets the media, and the import returns once it passes.
	#[tokio::test(start_paused = true)]
	async fn a_finished_import_serves_its_tracks_for_the_linger() {
		const LINGER: Duration = Duration::from_secs(30);
		let (publish, consumer) = ts_stdin();
		let mut catalogs = hang::catalog::Catalog::<()>::subscribe(&consumer).await.unwrap();

		#[cfg_attr(
			not(feature = "capture"),
			expect(
				irrefutable_let_patterns,
				reason = "`Source::Capture` exists only with the capture feature"
			)
		)]
		let Source::Stream { decoder, catalog } = publish.source else {
			panic!("expected a stream source");
		};
		let started = kio::clock::Instant::now();
		let publication = Publication {
			broadcast: publish.broadcast.clone(),
			linger: LINGER,
			..Default::default()
		};
		let mut import = std::pin::pin!(decode(decoder, catalog, BBB, publication));
		let name = tokio::select! {
			res = &mut import => panic!("the import returned before its linger: {res:?}"),
			name = async {
				loop {
					let catalog = catalogs.next().await.unwrap().expect("a catalog before the finish").value;
					if let Some(name) = catalog.video.renditions.keys().next() {
						break name.clone();
					}
				}
			} => name,
		};

		// Subscribe only now, as a subscriber that learned the catalog at EOF does.
		let track = consumer.track(&name).unwrap().subscribe(None);
		let frame = tokio::select! {
			res = &mut import => panic!("the import returned before its linger: {res:?}"),
			frame = async {
				let mut track = track.await.unwrap();
				let mut group = track.recv_group().await.unwrap().expect("a video group");
				group.read_frame().await.unwrap()
			} => frame,
		};
		assert!(frame.is_some(), "the finished track still serves its media");

		import.await.unwrap();
		assert!(started.elapsed() >= LINGER, "returned after {:?}", started.elapsed());
		assert!(consumer.is_closed(), "the broadcast ends with the linger");
	}

	/// A stdin import announces only once its input has declared its tracks, so whoever
	/// resolves the broadcast the moment it is announced finds them (#4352).
	#[tokio::test(start_paused = true)]
	async fn an_import_announces_once_its_input_declares_its_tracks() {
		// Longer than the two seconds a live TS catalog waits on to measure its mux rate.
		const TS: &[u8] = include_bytes!("../../moq-mux/src/container/ts/test_data/scte35/bbb5s.ts");
		const FMP4: &[u8] = include_bytes!("../../moq-mux/src/container/fmp4/test_data/av1.mp4");
		for (format, input) in [
			(PublishFormat::Ts { program: None }, TS),
			(PublishFormat::TsPassthrough { pcr_pid: None }, TS),
			(PublishFormat::Fmp4 { cmaf: false }, FMP4),
			(PublishFormat::Fmp4 { cmaf: true }, FMP4),
		] {
			let origin = moq_tokio::origin::spawn();
			let consumer = origin.consume();
			let mut announced = consumer.announced();
			let publish = Publish::stdin(
				&origin,
				"demo",
				moq_net::Instance::random(),
				&format,
				Default::default(),
			)
			.unwrap();
			// A TS import announces through its own route.
			let pending = publish.route.clone().map(|route| Pending::new(route).unwrap());
			#[cfg_attr(
				not(feature = "capture"),
				expect(
					irrefutable_let_patterns,
					reason = "`Source::Capture` exists only with the capture feature"
				)
			)]
			let Source::Stream { decoder, catalog } = publish.source else {
				panic!("expected a stream source");
			};
			let (mut stdin, input_end) = tokio::io::duplex(input.len());
			let publication = Publication {
				pending,
				..Default::default()
			};
			let mut import = std::pin::pin!(decode(decoder, catalog, input_end, publication));

			tokio::select! {
				res = &mut import => panic!("the import returned before its input ended: {res:?}"),
				event = kio::clock::timeout(Duration::from_secs(5), announced.next()) => {
					assert!(event.is_err(), "announced before any input: {event:?}");
				}
			}

			tokio::io::AsyncWriteExt::write_all(&mut stdin, input).await.unwrap();
			let resolved = tokio::select! {
				res = &mut import => panic!("the import returned before its input ended: {res:?}"),
				_ = kio::clock::sleep(Duration::from_secs(30)) => panic!("never announced"),
				resolved = async {
					let event = announced.next().await.expect("origin closed");
					assert!(matches!(event, moq_net::announce::Event::Present(_)), "{event:?}");
					let broadcast = consumer.request("demo", None).await.unwrap();
					let mut catalogs = hang::catalog::Catalog::<()>::subscribe(&broadcast).await.unwrap();
					let catalog = catalogs.next().await.unwrap().expect("a catalog").value;
					// A passthrough carries the multiplex on one track, named by its m2ts section.
					let name = match catalog.m2ts {
						Some(m2ts) => m2ts.track,
						None => catalog.video.renditions.keys().next().expect("a video rendition").clone(),
					};
					broadcast.track(&name).unwrap().subscribe(None).await
				} => resolved,
			};
			assert!(resolved.is_ok(), "the announced track serves: {:?}", resolved.err());

			drop(stdin);
			import.await.unwrap();
		}
	}

	#[tokio::test(start_paused = true)]
	async fn internal_stdin_failure_preserves_the_catalog_error() {
		struct Broken;
		impl tokio::io::AsyncRead for Broken {
			fn poll_read(
				self: std::pin::Pin<&mut Self>,
				_cx: &mut std::task::Context<'_>,
				_buf: &mut tokio::io::ReadBuf<'_>,
			) -> std::task::Poll<std::io::Result<()>> {
				std::task::Poll::Ready(Err(std::io::Error::other("stdin failed")))
			}
		}
		let (publish, consumer) = ts_stdin();
		let mut catalog_reader = consumer
			.track(hang::Catalog::DEFAULT_NAME)
			.unwrap()
			.subscribe(None)
			.await
			.unwrap();
		#[cfg_attr(
			not(feature = "capture"),
			expect(
				irrefutable_let_patterns,
				reason = "`Source::Capture` exists only with the capture feature"
			)
		)]
		let Source::Stream { decoder, catalog } = publish.source else {
			panic!("stream source");
		};
		let err = decode(decoder, catalog, Broken, Publication::default())
			.await
			.unwrap_err();
		assert_eq!(err.to_string(), "stdin failed");
		assert!(
			matches!(catalog_reader.recv_group().await, Err(moq_net::Error::Transport(ref reason)) if reason == "stdin failed")
		);
	}

	/// A source whose clock steps back republishes the path under a new instance, announced once
	/// its catalog goes out, with the sections the first instance's catalog carried. The new
	/// catalog reuses the first instance's mux rate, so an export of it needn't wait on a fresh
	/// measurement window.
	#[tokio::test(start_paused = true)]
	async fn a_clock_step_republishes_under_a_new_instance() {
		// 5 s of H.264 and AAC at a constant 400 kb/s, its clock on PID 0x100.
		const CLIP: &[u8] = include_bytes!("../../moq-mux/src/container/ts/test_data/bbb5s_cbr.ts");
		// Its first 1.5 s: short of a measurement window, so only a reused rate is in the catalog.
		const HEAD: &[u8] = CLIP.split_at(400 * 188).0;
		let mut flag = vec![0x47, 0x01, 0x00, 0x20, 183, 0x80];
		flag.resize(188, 0xff);

		let origin = moq_tokio::origin::spawn();
		let mut announced = origin.consume().announced();
		let first = moq_net::Instance::random();
		let publish = Publish::stdin(
			&origin,
			"demo",
			first.clone(),
			&PublishFormat::Ts { program: None },
			Default::default(),
		)
		.unwrap()
		.with_echo(hang::catalog::Echo::new(
			moq_net::path::Relative::new("viewer").unwrap(),
		))
		.unwrap();
		#[cfg_attr(
			not(feature = "capture"),
			expect(
				irrefutable_let_patterns,
				reason = "`Source::Capture` exists only with the capture feature"
			)
		)]
		let Source::Stream { decoder, catalog } = publish.source else {
			panic!("expected a stream source");
		};
		let publication = Publication {
			sections: publish.sections,
			..Default::default()
		};
		let (mut stdin, input_end) = tokio::io::duplex(2 * CLIP.len() + flag.len());
		let mut import = std::pin::pin!(decode(decoder, catalog, input_end, publication));

		// The instance of the next instance announced on the path.
		let mut next = async |import: &mut std::pin::Pin<&mut _>| {
			tokio::select! {
				res = import => panic!("the import returned before its input ended: {res:?}"),
				_ = kio::clock::sleep(Duration::from_secs(30)) => panic!("never announced"),
				instance = async {
					loop {
						use moq_net::announce::Event;
						if let Event::Present(announce) = announced.next().await.unwrap() {
							break announce.route.instance().clone();
						}
					}
				} => instance,
			}
		};
		tokio::io::AsyncWriteExt::write_all(&mut stdin, CLIP).await.unwrap();
		assert_eq!(next(&mut import).await, first);
		let broadcast = origin.consume().request("demo", None).await.unwrap();
		let mut catalogs = hang::catalog::Catalog::<ts::Ext>::subscribe(&broadcast).await.unwrap();
		let catalog = catalogs.next().await.unwrap().expect("a catalog").value;
		let rate = catalog
			.ext
			.mpegts
			.mux_rate
			.expect("the first instance measured its rate");
		drop(catalogs);

		// The encoder restarts its clock and plays the clip again: a rewind, flagged.
		tokio::io::AsyncWriteExt::write_all(&mut stdin, &[&flag[..], HEAD].concat())
			.await
			.unwrap();
		assert_ne!(next(&mut import).await, first, "the new instance has its own instance");
		let broadcast = origin.consume().request("demo", None).await.unwrap();
		let mut catalogs = hang::catalog::Catalog::<ts::Ext>::subscribe(&broadcast).await.unwrap();
		let catalog = catalogs.next().await.unwrap().expect("a catalog").value;
		assert!(catalog.echo.is_some(), "the new instance solicits feedback too");
		assert_eq!(
			catalog.ext.mpegts.mux_rate,
			Some(rate),
			"the new instance reuses the rate"
		);
		// Its first group is already out with the catalog.
		let (name, _) = catalog.video.renditions.iter().next().expect("a video rendition");
		let mut track = broadcast.track(name).unwrap().subscribe(None).await.unwrap();
		track
			.recv_group()
			.await
			.unwrap()
			.expect("the new instance's first group");
		drop(stdin);
		import.await.unwrap();
	}

	/// An unflagged rewind puts its leading PAT/PMT before the step. The replacement carries no
	/// program map over, so it starts at the source's next keyframe after the tables repeat. A
	/// live stitched export joins that first group, rather than waiting for a later one, and the
	/// old program drains through the cut.
	#[tokio::test(start_paused = true)]
	async fn a_stitched_republish_joins_the_first_group() {
		use mpeg2ts::pes::{PesPacketReader, ReadPesPacket};
		use mpeg2ts::ts::TsPacketReader;
		use std::task::Poll;

		const CLIP: &[u8] = include_bytes!("../../moq-mux/src/container/ts/test_data/bbb5s_cbr.ts");
		let origin = moq_tokio::origin::spawn();
		let mut announced = origin.consume().announced();
		let publish = Publish::stdin(
			&origin,
			"demo",
			moq_net::Instance::random(),
			&PublishFormat::Ts { program: None },
			Default::default(),
		)
		.unwrap();
		#[cfg_attr(
			not(feature = "capture"),
			expect(irrefutable_let_patterns, reason = "capture is disabled")
		)]
		let Source::Stream { decoder, catalog } = publish.source else {
			panic!("a stream source");
		};
		let mut instance = Instance {
			decoder,
			catalog,
			pending: None,
			control: None,
			broadcast: None,
			report: None,
		};
		let mut log = ts::stats::Log::default();
		let mut follower = None;
		let mut listed = false;
		let mut output = [Vec::new(), Vec::new()];
		for chunk in CLIP.iter().chain(CLIP).copied().collect::<Vec<_>>().chunks(8 * 188) {
			if let Some(republished) = feed(&mut instance.decoder, &mut instance.pending, chunk).unwrap() {
				instance
					.republished(&Sections::default(), republished, &mut log)
					.unwrap();
			}
			while let Some(event) = announced.try_next() {
				listed |= matches!(event, moq_net::announce::Event::Present(_));
			}
			if follower.is_none() && listed {
				let export = open_export(
					resolved(&origin, "demo").await,
					CatalogFormat::Hang,
					Duration::from_secs(1),
				)
				.await
				.unwrap();
				follower = Some(
					ts::Follower::new(export)
						.unwrap()
						.with_follow(Duration::from_secs(30))
						.with_stitch(true),
				);
			}
			if let Some(follower) = &mut follower {
				loop {
					match follower.poll_next(&kio::Waiter::noop()) {
						Poll::Ready(Ok(Some(frame))) => {
							output[follower.export().discontinuity() as usize].extend_from_slice(&frame.payload)
						}
						Poll::Ready(end) => panic!("a live feed ended: {end:?}"),
						Poll::Pending => break,
					}
				}
			}
			tokio::time::advance(Duration::from_micros(30_080)).await;
		}
		let Instance {
			decoder,
			mut catalog,
			control,
			broadcast,
			pending,
			report,
		} = instance;
		let decoder = decoder.finish().unwrap();
		catalog.finish().unwrap();
		drop((decoder, catalog, control, broadcast, pending, report));
		let mut follower = follower.unwrap();
		while let Some(frame) = follower.next().await.unwrap() {
			output[follower.export().discontinuity() as usize].extend_from_slice(&frame.payload);
		}
		assert_eq!(follower.export().discontinuity(), 1, "one program switch");
		let pictures = |bytes: &[u8]| {
			let mut reader = PesPacketReader::new(TsPacketReader::new(std::io::Cursor::new(bytes)));
			let mut pictures = Vec::new();
			while let Some(pes) = reader.read_pes_packet().unwrap() {
				if !pes.header.stream_id.is_video() {
					continue;
				}
				let mut payload = BytesMut::from(pes.data.as_slice());
				let mut nals = moq_mux::codec::annexb::NalIterator::new(&mut payload);
				// HEVC slices only: the export may repeat parameter sets at a keyframe.
				let mut slices = Vec::new();
				let mut keyframe = false;
				let mut all: Vec<_> = nals.by_ref().map(Result::unwrap).collect();
				all.extend(nals.flush().unwrap());
				for nal in all {
					let kind = (nal[0] >> 1) & 0x3f;
					if kind < 32 {
						keyframe |= (16..=23).contains(&kind);
						slices.push(nal);
					}
				}
				pictures.push((pes.header.pts.unwrap().as_u64(), keyframe, slices));
			}
			pictures
		};
		let source = pictures(CLIP);
		let keyframes: Vec<_> = source.iter().filter(|picture| picture.1).collect();
		assert!(
			keyframes.len() >= 2 && keyframes[0].2 != keyframes[1].2,
			"the clip has a second, distinct group"
		);
		let replacement = pictures(&output[1]);
		assert_eq!(
			replacement.first().map(|picture| &picture.2),
			Some(&keyframes[1].2),
			"the export joins the replacement's first group: the source's second keyframe"
		);
		// Compare picture bytes: Hang normalizes PTS to microseconds, which can lose a 90 kHz tick.
		assert_eq!(
			pictures(&output[0])
				.iter()
				.max_by_key(|picture| picture.0)
				.map(|picture| &picture.2),
			source.iter().max_by_key(|picture| picture.0).map(|picture| &picture.2),
			"the old program drains through the cut"
		);
	}

	/// Input that ends before declaring any track fails, rather than announcing a broadcast
	/// with nothing in it. These inputs hold only metadata (a PMT, an FLV header, an AUD)
	/// that configures no track.
	#[tokio::test(start_paused = true)]
	async fn an_import_with_no_track_fails_unannounced() {
		let tables = two_programs()[..3 * 188].to_vec();
		let flv_header = b"FLV\x01\x05\x00\x00\x00\x09\x00\x00\x00\x00".to_vec();
		let aud = vec![0, 0, 0, 1, 9, 0xf0];
		for (format, input) in [
			(PublishFormat::Ts { program: None }, Vec::new()),
			(PublishFormat::Ts { program: Some(1) }, tables),
			(PublishFormat::Flv, flv_header),
			(PublishFormat::Avc3, aud),
		] {
			let origin = moq_tokio::origin::spawn();
			let mut announced = origin.consume().announced();
			let publish = Publish::stdin(
				&origin,
				"demo",
				moq_net::Instance::random(),
				&format,
				Default::default(),
			)
			.unwrap();
			// A TS import announces through its own route.
			let pending = publish.route.clone().map(|route| Pending::new(route).unwrap());
			#[cfg_attr(
				not(feature = "capture"),
				expect(
					irrefutable_let_patterns,
					reason = "`Source::Capture` exists only with the capture feature"
				)
			)]
			let Source::Stream { decoder, catalog } = publish.source else {
				panic!("expected a stream source");
			};
			let publication = Publication {
				pending,
				..Default::default()
			};
			let err = decode(decoder, catalog, &input[..], publication).await.unwrap_err();
			assert!(err.to_string().contains("declared"), "{err:#}");
			let event = kio::clock::timeout(Duration::from_secs(1), announced.next()).await;
			assert!(event.is_err(), "announced an empty broadcast: {event:?}");
		}
	}

	/// A PAT listing two programs, then one MP2 PES of each: program 1 on PID `0x61` at 1 s
	/// with fill bytes `0xAA`/`0xBB`, program 2 on PID `0x71` an hour later with `0xCC`/`0xDD`.
	pub(crate) fn two_programs() -> Vec<u8> {
		use mpeg2ts::es::StreamType;
		use mpeg2ts::ts::payload::{Pat, Pmt};
		use mpeg2ts::ts::{
			ContinuityCounter, EsInfo, Pid, ProgramAssociation, TransportScramblingControl, TsHeader, TsPacket,
			TsPacketWriter, TsPayload, VersionNumber, WriteTsPacket,
		};

		let write = |out: &mut Vec<u8>, pid: u16, payload: TsPayload| {
			let packet = TsPacket {
				header: TsHeader {
					transport_error_indicator: false,
					transport_priority: false,
					pid: Pid::new(pid).unwrap(),
					transport_scrambling_control: TransportScramblingControl::NotScrambled,
					continuity_counter: ContinuityCounter::default(),
				},
				adaptation_field: None,
				payload: Some(payload),
			};
			TsPacketWriter::new(out).write_ts_packet(&packet).unwrap();
		};
		let programs = [
			(1, 0x100, 0x61, 90_000, [0xAA, 0xBB]),
			(2, 0x200, 0x71, 3_601 * 90_000, [0xCC, 0xDD]),
		];

		let mut out = Vec::new();
		let pat = Pat {
			transport_stream_id: 1,
			version_number: VersionNumber::default(),
			table: programs
				.iter()
				.map(|&(program_num, pmt_pid, ..)| ProgramAssociation {
					program_num,
					program_map_pid: Pid::new(pmt_pid).unwrap(),
				})
				.collect(),
		};
		write(&mut out, Pid::PAT, TsPayload::Pat(pat));
		for &(program_num, pmt_pid, es_pid, ..) in &programs {
			let pmt = Pmt {
				program_num,
				pcr_pid: Some(Pid::new(es_pid).unwrap()),
				version_number: VersionNumber::default(),
				program_info: Vec::new(),
				es_info: vec![EsInfo {
					stream_type: StreamType::Mpeg1Audio,
					elementary_pid: Pid::new(es_pid).unwrap(),
					descriptors: Vec::new(),
				}],
			};
			write(&mut out, pmt_pid, TsPayload::Pmt(pmt));
		}
		for &(_, _, es_pid, pts, fills) in &programs {
			out.extend(mp2_pes_packet(es_pid, pts, fills));
		}
		out
	}

	/// One TS packet on `pid` holding a PES of two MP2 frames at `pts`, the second confirming
	/// the first so the legacy path publishes it.
	fn mp2_pes_packet(pid: u16, pts: u64, fills: [u8; 2]) -> Vec<u8> {
		let mut payload = Vec::new();
		for fill in fills {
			let mut frame = vec![0xFF, 0xF5, 0x18, 0xC0];
			frame.resize(72, fill);
			payload.extend(frame);
		}
		let mut pes = vec![0x00, 0x00, 0x01, 0xc0];
		let len = 3 + 5 + payload.len();
		pes.extend([(len >> 8) as u8, len as u8, 0x80, 0x80, 0x05]);
		pes.extend([
			0x21 | (((pts >> 30) & 0x07) << 1) as u8,
			(pts >> 22) as u8,
			0x01 | (((pts >> 15) & 0x7f) << 1) as u8,
			(pts >> 7) as u8,
			0x01 | ((pts & 0x7f) << 1) as u8,
		]);
		pes.extend(payload);

		let stuffing = 184 - 1 - pes.len();
		let mut packet = vec![0x47, 0x40 | (pid >> 8) as u8, pid as u8, 0x30, stuffing as u8];
		if stuffing > 0 {
			packet.push(0x00);
			packet.resize(5 + stuffing, 0xff);
		}
		packet.extend(pes);
		assert_eq!(packet.len(), 188);
		packet
	}

	/// Plain `import ts` refuses a multiplex, naming its programs and the flag that picks one.
	#[test]
	fn a_multiplex_is_refused_with_the_programs_and_the_flag() {
		let (mut publish, _) = ts_stdin();
		#[cfg_attr(
			not(feature = "capture"),
			expect(
				irrefutable_let_patterns,
				reason = "`Source::Capture` exists only with the capture feature"
			)
		)]
		let Source::Stream { decoder, .. } = &mut publish.source else {
			panic!("expected a stream source");
		};
		let err = format!("{:#}", decoder.decode_chunk(&two_programs()).unwrap_err());
		assert!(err.contains("--program") && err.contains("programs (1, 2)"), "{err}");
	}

	/// Read the first frame of a verbatim track back as raw bytes.
	async fn read_frame(consumer: &moq_net::broadcast::Consumer, name: &str) -> Vec<u8> {
		let track = consumer.track(name).unwrap().subscribe(None).await.unwrap();
		let mut reader = Consumer::new(track, Container::Legacy(moq_mux::container::Kind::Data));
		let frame = kio::clock::timeout(Duration::from_secs(1), next_frame(&mut reader))
			.await
			.expect("verbatim read timed out");
		frame.payload.to_vec()
	}

	/// The next frame, passing over gaps and ends.
	async fn next_frame<C: moq_mux::container::Container>(reader: &mut Consumer<C>) -> moq_mux::container::Frame
	where
		C::Error: std::fmt::Debug,
	{
		loop {
			let read = reader.read().await.unwrap().expect("a published frame");
			if let moq_mux::container::Read::Frame { frame, .. } = read {
				return frame;
			}
		}
	}
}
