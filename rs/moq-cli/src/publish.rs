use anyhow::Context;
use hang::moq_net;
use moq_mux::container::{flv, fmp4, ts};

/// Container format read from stdin on the import (source) side.
#[derive(Clone, Copy)]
pub enum PublishFormat {
	/// Raw AVC (H.264) Annex B elementary stream.
	Avc3,
	/// Fragmented MP4 (CMAF), republished whole as CMAF when `passthrough`.
	Fmp4 { passthrough: bool },
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
	/// Capture and encode only while subscribed.
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
	/// `--microphone`, or no audio source flag, opens the default input.
	#[usage(long, group = "audio-source")]
	pub microphone: Option<Option<String>>,

	/// Capture the system (desktop) audio instead of a microphone: everything the
	/// machine is playing, minus this process. macOS only, and it needs the Screen
	/// Recording permission.
	#[usage(long, group = "audio-source")]
	pub system_audio: bool,

	/// Target audio bitrate in bits per second (Opus). Omit for the codec default.
	#[usage(long)]
	pub audio_bitrate: Option<u32>,

	/// Video mode: on keeps the recent past; demand captures only while subscribed;
	/// off publishes no video. Video probes its source once at startup even in demand mode.
	#[usage(long, value_enum, default = "on")]
	pub video: Mode,

	/// Audio mode: on keeps the recent past; demand opens the source only while
	/// subscribed; off publishes no audio.
	#[usage(long, value_enum, default = "on")]
	pub audio: Mode,

	/// The released spelling of `--video off`.
	#[usage(long = "no-video", hide = true)]
	no_video: bool,

	/// The released spelling of `--audio off`.
	#[usage(long = "no-audio", hide = true)]
	no_audio: bool,
}

enum PublishDecoder {
	Avc3 {
		split: Box<moq_mux::codec::h264::Split>,
		import: Box<moq_mux::codec::h264::Import>,
	},
	Fmp4(Box<fmp4::Import>),
	// TS carries undecoded elementary streams (SCTE-35, teletext, DVB AC-3, ...)
	// verbatim, so it uses the `mpegts` catalog extension rather than the media-only `()`.
	Ts(Box<ts::Import<ts::Ext>>),
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
	fn decode_chunk(&mut self, chunk: &[u8]) -> anyhow::Result<()> {
		match self {
			Self::Avc3 { split, import } => {
				let frames = split.decode(chunk, None)?;
				import.decode(frames)?;
			}
			Self::Fmp4(d) => d.decode(chunk)?,
			Self::Ts(d) => d.decode(chunk).map_err(suggest_program)?,
			Self::TsPrograms(d) => d.decode(chunk)?,
			Self::TsPassthrough(d) => d.decode(chunk)?,
			Self::Flv(d) => d.decode(chunk)?,
		}
		Ok(())
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
	///
	/// `import ts --program all` owns its broadcasts, so it returns them; see
	/// [`ts::Programs::finish`].
	fn finish(&mut self) -> anyhow::Result<Vec<moq_net::broadcast::Producer>> {
		match self {
			Self::Avc3 { split, import } => {
				let tail = split.flush(None)?;
				import.decode(tail)?;
				import.finish()?;
			}
			Self::Fmp4(d) => d.finish()?,
			Self::Ts(d) => d.finish()?,
			Self::TsPrograms(d) => return d.finish(),
			Self::TsPassthrough(d) => d.finish()?,
			Self::Flv(d) => d.finish()?,
		}
		Ok(Vec::new())
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

/// Point a multi-program refusal at the flag that resolves it.
fn suggest_program(err: anyhow::Error) -> anyhow::Error {
	if err.is::<ts::MultipleProgramsError>() {
		err.context("choose one with `--program <n>`, or publish each with `--program all`")
	} else {
		err
	}
}

/// The catalog a stdin decoder publishes into. TS carries the `mpegts` extension.
enum PublishCatalog {
	Media(moq_mux::catalog::Producer),
	Ts(moq_mux::catalog::Producer<ts::Ext>),
	/// `import ts --program all`: each program's catalog, finished by [`ts::Programs::finish`].
	TsPrograms,
}

impl PublishCatalog {
	fn abort(self, err: moq_net::Error) {
		match self {
			Self::Media(catalog) => catalog.abort(err),
			Self::Ts(catalog) => catalog.abort(err),
			Self::TsPrograms => {}
		}
	}

	/// End the catalog tracks cleanly, keeping the renditions they last listed.
	fn finish(&mut self) -> anyhow::Result<()> {
		match self {
			Self::Media(catalog) => catalog.finish()?,
			Self::Ts(catalog) => catalog.finish()?,
			Self::TsPrograms => {}
		}
		Ok(())
	}

	/// The multiplex rate a TS catalog records.
	fn mux_rate(&self) -> Option<u64> {
		match self {
			Self::Ts(catalog) => catalog.snapshot().ext.mpegts.mux_rate,
			Self::Media(_) | Self::TsPrograms => None,
		}
	}
}

/// Build the importer and `mpegts` catalog for one TS broadcast.
///
/// TS carries undecoded elementary streams (SCTE-35, teletext, DVB AC-3, ...) verbatim, so it
/// uses the `mpegts` catalog extension rather than the media-only `()`. The catalog producer
/// owns the broadcast's catalog tracks, so each broadcast gets exactly one.
fn ts_import(
	broadcast: &mut moq_net::broadcast::Producer,
	config: moq_mux::catalog::Config,
	program: Option<u16>,
	mux_rate: Option<u64>,
) -> anyhow::Result<(ts::Import<ts::Ext>, moq_mux::catalog::Producer<ts::Ext>)> {
	let mut initial = moq_mux::catalog::hang::Catalog::<ts::Ext>::default();
	// A rate already measured on this multiplex releases the catalog with the first media.
	initial.ext.mpegts.mux_rate = mux_rate;
	let config = config.with_catalog(initial);
	let catalog = moq_mux::catalog::Producer::new(broadcast, config)?;
	let mut import = ts::Import::new(broadcast.clone(), catalog.timebase().reserve());
	if let Some(program) = program {
		import = import.with_program(program);
	}
	Ok((import, catalog))
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
		audio: Option<moq_audio::encode::Capture>,
	},
}

/// A stdin or capture publisher: decodes stdin (or captures local devices) into
/// a broadcast that the MoQ side announces.
pub struct Publish {
	source: Source,
	// Keeps the fill surface until finish. `None` for `import ts --program all`,
	// whose programs each own their fill and publication control.
	broadcast: Option<moq_net::broadcast::Producer>,
	publication: Option<moq_net::origin::Publication>,
	// A stdin import's route, announced once its catalog lists the tracks; see [`Self::announce`].
	route: Option<moq_net::origin::Route>,
	// The `--report` track, written beside the decode loop; see [`Self::with_report`].
	report: Option<crate::report::Stats>,
	// How long a stdin import keeps its broadcast up after EOF; see [`Self::with_linger`].
	linger: std::time::Duration,
	// How a stdin TS import republishes when its source's clock steps; see [`Self::stdin`].
	republish: Option<Republish>,
}

/// A broadcast's announcement, held until its catalog first goes out.
///
/// The catalog withholds its first snapshot until the input names at least one servable track. A
/// relay queries a broadcast's tracks the moment it is announced, so a standby
/// announced any sooner can receive requests for tracks it cannot serve yet.
struct Pending {
	broadcast: moq_net::origin::Publication,
	catalog: moq_net::track::Consumer,
	route: moq_net::origin::Route,
}

impl Pending {
	fn new(broadcast: moq_net::origin::Publication, route: moq_net::origin::Route) -> anyhow::Result<Self> {
		let catalog = broadcast.consume().track(hang::Catalog::DEFAULT_NAME)?;
		Ok(Self {
			broadcast,
			catalog,
			route,
		})
	}

	/// Announce the broadcast if its catalog has gone out, returning whether it has.
	fn announce(&mut self) -> anyhow::Result<bool> {
		if !matches!(
			self.catalog.poll_largest(&moq_net::kio::Waiter::noop()),
			std::task::Poll::Ready(Some(_))
		) {
			return Ok(false);
		}
		self.broadcast
			.announce(self.route.clone())
			.context("failed to announce broadcast")?;
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
		// TS builds its `Ext` catalog instead of the shared `()` below.
		if let PublishFormat::Ts { program } = *format {
			let (ts, catalog) = ts_import(&mut broadcast, config, program, None)?;
			return Ok(Self {
				source: Source::Stream {
					decoder: PublishDecoder::Ts(Box::new(ts)),
					catalog: PublishCatalog::Ts(catalog),
				},
				broadcast: Some(broadcast),
				publication: None,
				route: None,
				report: None,
				linger: moq_mux::Linger::DEFAULT,
				republish: None,
			});
		}

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
			PublishFormat::Fmp4 { passthrough } => {
				let timebase = catalog.timebase();
				let fmp4 = fmp4::Import::new(broadcast.clone(), timebase.reserve()).with_passthrough(*passthrough);
				PublishDecoder::Fmp4(Box::new(fmp4))
			}
			PublishFormat::Ts { .. } => unreachable!("TS is handled above with the mpegts catalog extension"),
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
				catalog: PublishCatalog::Media(catalog),
			},
			broadcast: Some(broadcast),
			publication: None,
			route: None,
			report: None,
			linger: moq_mux::Linger::DEFAULT,
			republish: None,
		})
	}

	/// Build a publisher decoding `format` from stdin into a new broadcast at `name` on
	/// `origin`, retaining its announcement control alongside the decoder's fill handle.
	///
	/// A TS import whose source's clock steps onto another timeline ([`ts::Restart`]), such as
	/// a changeover between two encoders whose clocks were never locked, finishes the broadcast
	/// and republishes the rest of the input at `name` under a new epoch.
	pub(super) fn stdin(
		origin: &moq_net::origin::Producer,
		name: &str,
		format: &PublishFormat,
		config: moq_mux::catalog::Config,
	) -> anyhow::Result<Self> {
		let publication = origin.create_broadcast(name).context("failed to create broadcast")?;
		let republish = match *format {
			PublishFormat::Ts { program } => Some(Republish {
				origin: origin.clone(),
				name: name.to_string(),
				config: config.clone(),
				program,
				report: None,
				echo: None,
			}),
			_ => None,
		};
		let mut result = Self::new(publication.broadcast().clone(), format, config)?;
		result.publication = Some(publication);
		result.republish = republish;
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
				catalog: PublishCatalog::TsPrograms,
			},
			broadcast: None,
			publication: None,
			route: None,
			report: None,
			linger: moq_mux::Linger::DEFAULT,
			republish: None,
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
		publication: moq_net::origin::Publication,
		args: &CaptureArgs,
		bandwidth: moq_net::bandwidth::Allocator,
		max_age: Option<std::time::Duration>,
	) -> anyhow::Result<Self> {
		let mut broadcast = publication.broadcast().clone();
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
				catalog,
				video,
				audio,
			},
			broadcast: Some(broadcast),
			publication: Some(publication),
			route: None,
			report: None,
			linger: moq_mux::Linger::DEFAULT,
			republish: None,
		})
	}

	/// Name a media report track in the catalog's `stats` section and write it beside the
	/// decode loop: what each track has written, and the transport of `connection` when there
	/// is one.
	///
	/// Call it before the importer declares its tracks, so the section rides the first catalog.
	pub fn with_report(mut self, connection: Option<moq_tokio::connection::Monitor>) -> anyhow::Result<Self> {
		let (Some(broadcast), Source::Stream { catalog, .. }) = (self.broadcast.as_mut(), &self.source) else {
			anyhow::bail!("--report reports a stdin import's one catalog");
		};
		if let Some(republish) = &mut self.republish {
			republish.report = Some(connection.clone());
		}
		let report = match catalog {
			PublishCatalog::Media(catalog) => crate::report::Stats::new(broadcast, catalog, connection)?,
			PublishCatalog::Ts(catalog) => crate::report::Stats::new(broadcast, catalog, connection)?,
			PublishCatalog::TsPrograms => anyhow::bail!("`import ts --program all` builds a catalog per program"),
		};
		self.report = Some(report);
		Ok(self)
	}

	/// Solicit viewer feedback under `echo` in the catalog.
	///
	/// Call it before the importer declares its tracks, so the section rides the first catalog.
	pub fn with_echo(mut self, echo: hang::catalog::Echo) -> anyhow::Result<Self> {
		let catalog = match &mut self.source {
			Source::Stream { catalog, .. } => catalog,
			#[cfg(feature = "capture")]
			Source::Capture { .. } => anyhow::bail!("--echo-path solicits feedback in a stdin import's one catalog"),
		};
		if let Some(republish) = &mut self.republish {
			republish.echo = Some(echo.clone());
		}
		match catalog {
			PublishCatalog::Media(catalog) => catalog.mutate(|catalog| catalog.echo = Some(echo))?,
			PublishCatalog::Ts(catalog) => catalog.mutate(|catalog| catalog.echo = Some(echo))?,
			PublishCatalog::TsPrograms => anyhow::bail!("`import ts --program all` builds a catalog per program"),
		}
		Ok(self)
	}

	/// Keep a stdin import's broadcast up for `linger` after EOF, so a subscriber that read the
	/// final catalog late still gets the tail. Zero closes it at EOF.
	pub fn with_linger(mut self, linger: std::time::Duration) -> Self {
		self.linger = linger;
		self
	}

	/// Advertise the broadcast's path under `epoch`.
	///
	/// A capture announces now. A stdin import announces from [`run`](Self::run), once the input
	/// has declared its tracks: see [`Pending`].
	pub fn announce(&mut self, epoch: moq_net::Epoch) -> anyhow::Result<()> {
		let route = moq_net::origin::Route::default().with_epoch(epoch);
		match (&self.source, &self.publication) {
			(_, None) => {}
			(Source::Stream { .. }, Some(_)) => self.route = Some(route),
			#[cfg(feature = "capture")]
			(Source::Capture { .. }, Some(broadcast)) => broadcast.announce(route).context("failed to announce broadcast")?,
		}
		Ok(())
	}

	/// Drive the source until stdin EOF (or the capture devices stop).
	pub async fn run(self) -> anyhow::Result<()> {
		match self.source {
			Source::Stream { decoder, catalog } => {
				let pending = match (&self.publication, self.route) {
					(Some(broadcast), Some(route)) => Some(Pending::new(broadcast.clone(), route)?),
					_ => None,
				};
				let publication = Publication {
					control: self.publication,
					broadcast: self.broadcast,
					pending,
					report: self.report,
					linger: self.linger,
					republish: self.republish,
				};
				decode(decoder, catalog, tokio::io::stdin(), publication).await
			}
			#[cfg(feature = "capture")]
			Source::Capture {
				broadcast,
				catalog,
				video,
				audio,
			} => {
				// Each enabled medium publishes its own track onto the shared
				// broadcast + catalog. Both stamp frames on the catalog's
				// advertised clock, so HLS/DASH wall times match the mapping on
				// the wire. Both capture for the whole run, or only while
				// subscribed in demand mode. Both run on this task rather
				// than a spawn: on macOS the audio future holds ObjC handles
				// across an await, so it is `!Send`.
				let video_fut = {
					let broadcast = broadcast.clone();
					let catalog = catalog.clone();
					async move {
						match video {
							Some(options) => moq_video::encode::publish_capture(broadcast, catalog, options)
								.await
								.map_err(anyhow::Error::from),
							None => Ok(()),
						}
					}
				};
				let audio_fut = async move {
					match audio {
						Some(options) => moq_audio::encode::publish_capture(broadcast, catalog, options)
							.await
							.map_err(anyhow::Error::from),
						None => Ok(()),
					}
				};

				tokio::try_join!(video_fut, audio_fut)?;
				Ok(())
			}
		}
	}
}

/// What a stdin import does around its media: announce, report, and retain the end.
#[derive(Default)]
struct Publication {
	/// The announcement control, held while the broadcast is up so its path stays announced.
	control: Option<moq_net::origin::Publication>,
	/// The broadcast to keep up after EOF, or `None` when the decoder owns its broadcasts.
	broadcast: Option<moq_net::broadcast::Producer>,
	/// The announcement held until the catalog first goes out.
	pending: Option<Pending>,
	/// `--report`, written beside the read loop, so a stalled input keeps reporting, and ended
	/// with its final counts when the media tracks are.
	report: Option<crate::report::Stats>,
	/// How long the finished broadcasts stay up after EOF.
	linger: std::time::Duration,
	/// How a step of the source's clock republishes, or `None` to end the import there.
	republish: Option<Republish>,
}

/// How a stdin TS import republishes its path when the source's clock steps onto another
/// timeline ([`ts::Restart`]): as a new broadcast under a new epoch, with the same catalog
/// sections.
struct Republish {
	origin: moq_net::origin::Producer,
	name: String,
	config: moq_mux::catalog::Config,
	program: Option<u16>,
	/// The connection `--report` measures, when the import reports.
	report: Option<Option<moq_tokio::connection::Monitor>>,
	echo: Option<hang::catalog::Echo>,
}

impl Republish {
	/// A new instance of the broadcast under a new epoch, announced once its catalog goes out.
	///
	/// `mux_rate` is the previous instance's measurement of the same multiplex, which the new
	/// catalog carries from its first media rather than waiting on a fresh window.
	fn instance(&self, mux_rate: Option<u64>) -> anyhow::Result<Instance> {
		let control = self
			.origin
			.create_broadcast(&self.name)
			.context("failed to create broadcast")?;
		let mut broadcast = control.broadcast().clone();
		let (import, mut catalog) = ts_import(&mut broadcast, self.config.clone(), self.program, mux_rate)?;
		// Before the import declares a track, so the sections ride the first catalog.
		let report = match &self.report {
			Some(connection) => Some(crate::report::Stats::new(&mut broadcast, &catalog, connection.clone())?),
			None => None,
		};
		if let Some(echo) = &self.echo {
			catalog.mutate(|catalog| catalog.echo = Some(echo.clone()))?;
		}
		let epoch = moq_net::Epoch::mint();
		tracing::info!(%epoch, "publisher instance");
		let pending = Pending::new(control.clone(), moq_net::origin::Route::default().with_epoch(epoch))?;
		Ok(Instance {
			decoder: PublishDecoder::Ts(Box::new(import)),
			catalog: PublishCatalog::Ts(catalog),
			control: Some(control),
			broadcast: Some(broadcast),
			pending: Some(pending),
			report,
		})
	}
}

/// One publisher instance of a stdin import: the decoder filling its broadcast, and what
/// announces and reports it.
struct Instance {
	decoder: PublishDecoder,
	catalog: PublishCatalog,
	control: Option<moq_net::origin::Publication>,
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
/// A step of the source's clock ends the instance there. With `republish`, the rest of the
/// input publishes a new instance on the same path; otherwise the import fails with it.
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
		republish,
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
		let restart = match instance.read(&mut reader).await {
			Ok(Some(restart)) => restart,
			Ok(None) => break Ok(()),
			Err(err) => break Err(err),
		};
		let Some(republish) = &republish else {
			break Err(restart.into());
		};
		if let Err(err) = instance.restart(republish, restart, &mut reader.log) {
			break Err(err);
		}
	};
	instance.end(result, &mut reader.log, linger).await
}

impl Instance {
	/// Read and decode until EOF, returning `None`, or until a step of the source's clock ends
	/// the instance.
	async fn read<R: tokio::io::AsyncRead + Unpin>(
		&mut self,
		reader: &mut Reader<R>,
	) -> anyhow::Result<Option<ts::Restart>> {
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
				if let Some(restart) = feed(decoder, pending, &reader.buffer)? {
					return Ok(Some(restart));
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

	/// The source's clock stepped: finish this instance before the step, and replace it with a
	/// new one on the same path that takes the rest of the input.
	fn restart(&mut self, republish: &Republish, restart: ts::Restart, log: &mut ts::stats::Log) -> anyhow::Result<()> {
		let mut restart = restart;
		loop {
			tracing::warn!(step = %restart.step, "the source's clock stepped; republishing");
			self.decoder.finish()?;
			if let Some(stats) = self.decoder.stats() {
				self.report_mpegts(&stats);
				log.finish(&stats);
			}
			if let Some(report) = &mut self.report {
				report.finish();
			}
			self.catalog.finish()?;
			// The finished broadcast ends as its handles drop, withdrawing its announcement.
			*self = republish.instance(self.catalog.mux_rate())?;
			*log = ts::stats::Log::default();
			match feed(&mut self.decoder, &mut self.pending, &restart.remainder)? {
				Some(next) => restart = next,
				None => return Ok(()),
			}
		}
	}

	/// Set the report's `mpegts` section, when the import reports.
	fn report_mpegts(&self, snapshot: &ts::stats::Snapshot) {
		if let Some(report) = &self.report {
			report.sections().set(|report| report.mpegts = Some(snapshot.clone()));
		}
	}

	/// Finish on a clean EOF and serve the finished broadcasts for the linger, or abort with
	/// the real cause of `result`, so subscribers see it instead of a bare Error::Dropped.
	async fn end(
		mut self,
		result: anyhow::Result<()>,
		log: &mut ts::stats::Log,
		linger: std::time::Duration,
	) -> anyhow::Result<()> {
		// Flush on a clean EOF; on any error (read, decode, or the flush
		// itself) abort with the real cause.
		let outcome = result.and_then(|()| self.decoder.finish());
		// The drain can release the first catalog. Without one the input declared no track, and
		// announcing would only advertise an empty broadcast.
		let outcome = outcome.and_then(|finished| {
			if let Some(announce) = &mut self.pending
				&& !announce.announce()?
			{
				anyhow::bail!("input ended before it declared a track");
			}
			Ok(finished)
		});
		// The drain at end of input can publish a frame nothing vouched for, so the
		// final snapshot is only complete after `finish`.
		if let Some(stats) = self.decoder.stats() {
			self.report_mpegts(&stats);
			log.finish(&stats);
		}
		let Self {
			decoder,
			mut catalog,
			control,
			broadcast,
			report,
			..
		} = self;
		match outcome {
			Ok(mut finished) => {
				// Finished before the broadcast is, so it lingers with the media.
				let mut report = report;
				if let Some(report) = &mut report {
					report.finish();
				}
				catalog.finish()?;
				finished.extend(broadcast);
				tracing::info!(?linger, "input ended; serving the finished broadcast");
				// The decoder and catalog hold the finished tracks, so they go only once it closes.
				moq_mux::Linger::new(finished, (decoder, catalog, report, control), linger)
					.wait()
					.await;
				Ok(())
			}
			Err(err) => {
				let reason = moq_net::Error::Transport(err.to_string());
				if let Some(report) = report {
					report.abort(reason.clone());
				}
				catalog.abort(reason.clone());
				decoder.abort(reason);
				Err(err)
			}
		}
	}
}

/// Decode `chunk` into `decoder`, announcing `pending` once its catalog goes out, or return the
/// step of the source's clock that ended the instance.
fn feed(
	decoder: &mut PublishDecoder,
	pending: &mut Option<Pending>,
	chunk: &[u8],
) -> anyhow::Result<Option<ts::Restart>> {
	if let Err(err) = decoder.decode_chunk(chunk) {
		return err.downcast::<ts::Restart>().map(Some);
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
				Some(id) => Source::Display(Some(id.clone())),
				// Wayland has no main display to name, so the system picker chooses the screen.
				#[cfg(all(target_os = "linux", feature = "pipewire"))]
				None if std::env::var_os("WAYLAND_DISPLAY").is_some() => {
					use moq_video::capture::portal::{Kind, Selection};
					Source::Portal(Selection::new(Kind::Screen))
				}
				None => Source::Display(None),
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
		config.cursor = !self.no_cursor;
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
	/// it through the catalog either way.
	fn audio_encode(&self, bandwidth: moq_net::bandwidth::Allocator) -> moq_audio::encode::Options {
		let mut options = moq_audio::encode::Options::default();
		options.config.bitrate = self
			.audio_bitrate
			.map(|bps| moq_net::bandwidth::Rate::from_bps(bps.into()));
		options.bandwidth = bandwidth;
		options
	}

	/// The audio counterpart to [`video_capture`](Self::video_capture).
	fn audio_capture(&self, bandwidth: moq_net::bandwidth::Allocator) -> moq_audio::encode::Capture {
		let mut options = moq_audio::encode::Capture::default();
		options.capture = self.audio_config();
		options.encode = self.audio_encode(bandwidth);
		options.on_demand = self.audio == Mode::Demand;
		options
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
		assert!(!args.audio_capture(Allocator::unlimited()).on_demand);

		for (video, audio) in [("on", "demand"), ("demand", "on"), ("demand", "demand")] {
			let args = capture_args(&["--video", video, "--audio", audio]);
			args.validate().unwrap();
			assert_eq!(
				args.video_capture(Allocator::unlimited()).unwrap().on_demand,
				video == "demand"
			);
			assert_eq!(args.audio_capture(Allocator::unlimited()).on_demand, audio == "demand");
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
	) -> moq_mux::Result<Export<moq_mux::catalog::Consumer<tscat::Ext>>> {
		let catalog = source.catalog::<tscat::Ext>(format).await?;
		Ok(Export::new(source, catalog))
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
		let broadcast = origin.create_broadcast("cli").unwrap();
		broadcast.announce(Default::default()).unwrap();
		settle().await;
		let config = moq_mux::catalog::Config::default().with_catalog(Catalog::<tscat::Ext>::default());
		let mut catalog = moq_mux::catalog::Producer::new(&mut broadcast.broadcast().clone(), config).unwrap();
		// Reserve before the hand-written tracks below, so their catalog edits are withheld and
		// the import still places the clock, keeping bbb's PTS those tracks are stamped against.
		let reserved = catalog.reserve();

		// Section-framed verbatim stream (SCTE-35, stream_type 0x86).
		let section = broadcast
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
		section_producer.finish().unwrap();

		// Whole-PES stream (undecoded private data, stream_type 0x06).
		let pes = broadcast
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
		pes_producer.finish().unwrap();

		// Add the real video/audio (moves `broadcast` into the importer).
		let mut import = Import::new(broadcast.broadcast().clone(), reserved);
		import.decode(&BytesMut::from(BBB)).unwrap();
		import.finish().unwrap();

		// `catalog`, the producers, and `import` stay alive: the exporter subscribes to
		// the retained tracks.
		drain(
			open_export(resolved(&origin, "cli").await, CatalogFormat::Hang)
				.await
				.unwrap()
				.with_delay(RECORDING_MAX_AGE),
		)
		.await
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

		// Publish side: `Publish::new(Ts)` builds a `ts::Import<Ext>`, so the verbatim
		// streams land in the broadcast instead of being dropped by the media-only path.
		// The broadcast is created on a throwaway origin so the exporter can resolve it by path.
		let origin = moq_tokio::origin::spawn();
		let mut publish =
			Publish::stdin(&origin, "cli", &PublishFormat::Ts { program: None }, Default::default()).unwrap();
		publish
			.publication
			.as_ref()
			.unwrap()
			.announce(Default::default())
			.unwrap();
		settle().await;
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
		decoder.decode_chunk(&input).unwrap();
		decoder.finish().unwrap();

		// Subscribe side: the same catalog stream `run_ts` selects, re-emitting the
		// ancillary streams verbatim.
		let output = drain(
			open_export(resolved(&origin, "cli").await, format)
				.await
				.unwrap()
				.with_delay(RECORDING_MAX_AGE),
		)
		.await;

		// Re-import the round-tripped TS and inspect the recovered `mpegts` section.
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let consumer = broadcast.consume();
		let config = moq_mux::catalog::Config::default().with_catalog(Catalog::<tscat::Ext>::default());
		let catalog = moq_mux::catalog::Producer::new(&mut broadcast, config).unwrap();
		let mut import = Import::new(broadcast, catalog.reserve());
		import.decode(&BytesMut::from(&output[..])).unwrap();
		import.finish().unwrap();
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
		let mut publish = Publish::new(
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
		let Source::Stream { decoder, .. } = &mut publish.source else {
			panic!("expected a stream source");
		};
		for chunk in KYRION.chunks(64 * 1024) {
			decoder.decode_chunk(chunk).unwrap();
		}
		decoder.finish().unwrap();

		let catalog = hang::catalog::Catalog::<()>::subscribe(&consumer)
			.await
			.unwrap()
			.next()
			.await
			.unwrap()
			.expect("a catalog");
		assert!(catalog.video.renditions.is_empty() && catalog.audio.renditions.is_empty());
		let m2ts = catalog.m2ts.expect("the m2ts section");
		assert!(m2ts.random_access);

		let mut track = consumer
			.track(&m2ts.track)
			.unwrap()
			.subscribe(moq_net::track::Subscription::default().with_max_delay(RECORDING_MAX_AGE))
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

		let _ = moq_tokio::crypto::install_default();
		for version in ["moq-lite-06", "moq-lite-07-wip", "moq-transport-18"] {
			let fixture = moq_relay::test_relay().await.unwrap();
			let ready = fixture.relay.ready();
			tokio::spawn(fixture.relay.run());
			ready.wait().await.unwrap();
			let net = crate::Net {
				quic: Default::default(),
				#[cfg(feature = "iroh")]
				iroh: None,
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
			let mut publish = Publish::stdin(
				&origin,
				"pt.hang",
				&PublishFormat::TsPassthrough { pcr_pid: None },
				Default::default(),
			)
			.unwrap();
			publish
				.publication
				.as_ref()
				.unwrap()
				.announce(Default::default())
				.unwrap();
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
				.unwrap();
			let mut catalogs = hang::catalog::Catalog::<()>::subscribe(&consumer).await.unwrap();
			let catalog = kio::clock::timeout(WAIT, catalogs.next())
				.await
				.unwrap()
				.unwrap()
				.expect("a catalog");
			let name = catalog.m2ts.expect("the m2ts section").track;

			// Subscribe and take the first group, so a read starts behind anything decoded
			// afterwards: an IETF subscription joins the publisher's current group when its
			// SUBSCRIBE arrives, not when it is sent.
			let start = async |subscription: moq_net::track::Subscription| {
				let mut track = consumer.track(&name).unwrap().subscribe(subscription).await.unwrap();
				let first = kio::clock::timeout(WAIT, track.recv_group()).await.unwrap().unwrap();
				(track, first)
			};
			// Each group's sequence and frames, read until the track ends. A read cut off as
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
						groups.push((group.sequence, frames));
						next = track.recv_group().await.unwrap();
					}
					groups.sort_by_key(|(sequence, _)| *sequence);
					groups
				}))
			};
			let recording =
				read(start(moq_net::track::Subscription::default().with_max_delay(RECORDING_MAX_AGE)).await);
			// All at once: the track ends while the relay is still pulling the older groups.
			decoder.decode_chunk(&second).unwrap();
			decoder.finish().unwrap();

			let recording: Vec<_> = recording
				.await
				.unwrap()
				.expect("the recording ends with the track")
				.into_iter()
				.map(|(sequence, frames)| (sequence, frames.expect("the recording reads every group whole")))
				.collect();
			let output: Vec<u8> = recording
				.iter()
				.flat_map(|(_, frames)| frames.iter().flat_map(|frame| frame.payload.iter().copied()))
				.collect();
			assert!(output.len() > KYRION.len(), "{version}: both copies crossed");
			assert!(
				input.ends_with(&output),
				"{version}: byte-identical from the first group"
			);
			let media: Vec<_> = recording.iter().filter(|(_, frames)| !frames.is_empty()).collect();
			let first = |frames: &[hang::container::Frame]| frames[0].timestamp.as_micros();
			let jump = media
				.windows(2)
				.find(|pair| first(&pair[1].1) > first(&pair[0].1) + 10_000_000)
				.map(|pair| pair[1].0)
				.unwrap_or_else(|| panic!("{version}: no jump crossed"));
			let last = recording.last().unwrap().0;
			assert_eq!(
				recording.iter().map(|(sequence, _)| *sequence).collect::<Vec<_>>(),
				(recording[0].0..=last).collect::<Vec<_>>(),
				"{version}: no group missing"
			);
			let markers: Vec<u64> = recording
				.iter()
				.filter(|(_, frames)| frames.is_empty())
				.map(|(sequence, _)| *sequence)
				.collect();
			assert_eq!(markers, [jump - 1], "{version}: the break marker precedes the jump");

			// Joining once every group is out, with a budget half the jump, reads every group
			// after it past the marker, and none of the groups the jump left more than
			// the budget behind. An IETF subscription starts at the newest group, so only lite
			// has the groups behind it to read, and a lite-06 subscriber drops the timestamps
			// that carry the jump.
			if version == "moq-lite-07-wip" {
				let late =
					read(start(moq_net::track::Subscription::default().with_max_delay(Duration::from_secs(10))).await)
						.await
						.unwrap()
						.expect("the late read ends with the track");
				let whole: Vec<u64> = late
					.iter()
					.filter(|(_, frames)| frames.is_some())
					.map(|(sequence, _)| *sequence)
					.collect();
				assert!(
					whole.ends_with(&(jump..=last).collect::<Vec<_>>()),
					"{version}: a late subscriber reads every group from the jump on: {whole:?}"
				);
				assert!(
					whole.iter().all(|sequence| *sequence + 2 >= jump),
					"{version}: only the group the jump follows is in budget before it: {whole:?}"
				);
			}
		}
	}

	/// `moq import ts` publishes the feed's own PTS (1.4s into bbb) and anchors the catalog
	/// clock on it, so the first frame is live on arrival.
	#[tokio::test]
	async fn ts_import_anchors_the_catalog_clock_on_arrival() {
		let broadcast = moq_net::broadcast::Info::new().produce();
		let consumer = broadcast.consume();
		let mut publish = Publish::new(broadcast, &PublishFormat::Ts { program: None }, Default::default()).unwrap();
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
		let before = kio::clock::wall();
		decoder.decode_chunk(BBB).unwrap();
		let after = kio::clock::wall();
		decoder.finish().unwrap();

		let catalog = hang::catalog::Catalog::<()>::subscribe(&consumer)
			.await
			.unwrap()
			.next()
			.await
			.unwrap()
			.expect("a catalog");
		let clock = catalog.clock.expect("the catalog advertises a clock");
		let (name, config) = catalog.video.renditions.iter().next().expect("a video rendition");
		let track = consumer.track(name).unwrap().subscribe(None).await.unwrap();
		let container = moq_mux::catalog::hang::Container::try_from(config).unwrap();
		let first = next_frame(&mut Consumer::new(track, container)).await.timestamp;
		assert!(
			first.as_micros() < Duration::from_secs(5).as_micros(),
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
		let broadcast = moq_net::broadcast::Info::new().produce();
		let consumer = broadcast.consume();
		let echo = hang::catalog::Echo::new(moq_net::path::Relative::new("viewers").unwrap());
		let mut publish = Publish::new(broadcast, &PublishFormat::Ts { program: None }, Default::default())
			.unwrap()
			.with_echo(echo.clone())
			.unwrap();
		let mut track = consumer.track(Catalog::DEFAULT_NAME).unwrap();
		let mut published = || {
			matches!(
				track.poll_largest(&moq_net::kio::Waiter::noop()),
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
		let Source::Stream { decoder, .. } = &mut publish.source else {
			panic!("expected a stream source");
		};
		decoder.decode_chunk(BBB).unwrap();
		decoder.finish().unwrap();
		assert!(published(), "the input's tracks release the catalog");
		let catalog = Catalog::<()>::subscribe(&consumer)
			.await
			.unwrap()
			.next()
			.await
			.unwrap()
			.expect("a catalog");
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

		let broadcast = moq_net::broadcast::Info::new().produce();
		let publish = Publish::new(broadcast, &PublishFormat::Ts { program: None }, Default::default()).unwrap();
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
		for copies in [1, 16, 128] {
			let mut file = tempfile::NamedTempFile::new().unwrap();
			file.write_all(INPUT).unwrap();
			for _ in 1..copies {
				file.write_all(&padding).unwrap();
			}
			file.flush().unwrap();
			let input = tokio::fs::File::open(file.path()).await.unwrap();
			let broadcast = moq_net::broadcast::Info::new().produce();
			let publish = Publish::new(broadcast, &PublishFormat::Ts { program: None }, Default::default()).unwrap();
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
			println!(
				"bytes={bytes}: {elapsed:?}, {:.2} Mbit/s",
				bytes as f64 * 8.0 / elapsed.as_secs_f64() / 1e6
			);
		}
	}

	/// At stdin EOF the catalog ends cleanly and still lists the renditions, so a
	/// subscriber reads a finished broadcast rather than its tracks being removed.
	#[tokio::test(start_paused = true)]
	async fn eof_finishes_the_catalog_with_its_renditions() {
		let broadcast = moq_net::broadcast::Info::new().produce();
		let consumer = broadcast.consume();
		let publish = Publish::new(broadcast, &PublishFormat::Ts { program: None }, Default::default()).unwrap();
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
				Some(catalog) => last = Some(catalog),
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
		let broadcast = moq_net::broadcast::Info::new().produce();
		let consumer = broadcast.consume();
		let publish = Publish::new(broadcast, &PublishFormat::Ts { program: None }, Default::default()).unwrap();
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
					let catalog = catalogs.next().await.unwrap().expect("a catalog before the finish");
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
			(PublishFormat::Fmp4 { passthrough: false }, FMP4),
			(PublishFormat::Fmp4 { passthrough: true }, FMP4),
		] {
			let origin = moq_tokio::origin::spawn();
			let consumer = origin.consume();
			let mut announced = consumer.announced();
			let publish = Publish::stdin(&origin, "demo", &format, Default::default()).unwrap();
			let broadcast = publish.publication.clone().unwrap();
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
			let route = moq_net::origin::Route::default().with_epoch(moq_net::Epoch::mint());
			let pending = Pending::new(broadcast, route).unwrap();
			let (mut stdin, input_end) = tokio::io::duplex(input.len());
			let publication = Publication {
				pending: Some(pending),
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
					assert!(matches!(event, moq_net::announce::Event::Start(_)), "{event:?}");
					let broadcast = consumer.request("demo", None).await.unwrap();
					let mut catalogs = hang::catalog::Catalog::<()>::subscribe(&broadcast).await.unwrap();
					let catalog = catalogs.next().await.unwrap().expect("a catalog");
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
		let broadcast = moq_net::broadcast::Info::new().produce();
		let publish = Publish::new(
			broadcast.clone(),
			&PublishFormat::Ts { program: None },
			Default::default(),
		)
		.unwrap();
		let mut catalog_reader = broadcast
			.consume()
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

	/// A source whose clock steps back republishes the path under a new epoch, announced once
	/// its catalog goes out, with the sections the first instance's catalog carried. The new
	/// catalog reuses the first instance's mux rate, so it goes out with the first media rather
	/// than after a fresh two-second measurement window.
	#[tokio::test(start_paused = true)]
	async fn a_clock_step_republishes_under_a_new_epoch() {
		// 5 s of H.264 and AAC at a constant 400 kb/s, its clock on PID 0x100.
		const CLIP: &[u8] = include_bytes!("../../moq-mux/src/container/ts/test_data/bbb5s_cbr.ts");
		// Its first 1.5 s: short of a measurement window, so only a reused rate releases a catalog.
		const HEAD: &[u8] = CLIP.split_at(400 * 188).0;
		let mut flag = vec![0x47, 0x01, 0x00, 0x20, 183, 0x80];
		flag.resize(188, 0xff);

		let origin = moq_tokio::origin::spawn();
		let mut announced = origin.consume().announced();
		let mut publish = Publish::stdin(
			&origin,
			"demo",
			&PublishFormat::Ts { program: None },
			Default::default(),
		)
		.unwrap()
		.with_echo(hang::catalog::Echo::new(
			moq_net::path::Relative::new("viewer").unwrap(),
		))
		.unwrap();
		let first = moq_net::Epoch::mint();
		publish.announce(first.clone()).unwrap();
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
		let control = publish.publication.unwrap();
		let publication = Publication {
			pending: Some(Pending::new(control.clone(), publish.route.unwrap()).unwrap()),
			control: Some(control),
			broadcast: publish.broadcast,
			republish: publish.republish,
			..Default::default()
		};
		let (mut stdin, input_end) = tokio::io::duplex(2 * CLIP.len() + flag.len());
		let mut import = std::pin::pin!(decode(decoder, catalog, input_end, publication));

		// The epoch of the next instance announced on the path.
		let mut next = async |import: &mut std::pin::Pin<&mut _>| {
			tokio::select! {
				res = import => panic!("the import returned before its input ended: {res:?}"),
				_ = kio::clock::sleep(Duration::from_secs(30)) => panic!("never announced"),
				epoch = async {
					loop {
						use moq_net::announce::Event;
						if let Event::Start(announce) | Event::Restart(announce) = announced.next().await.unwrap() {
							break announce.route.epoch.unwrap();
						}
					}
				} => epoch,
			}
		};
		tokio::io::AsyncWriteExt::write_all(&mut stdin, CLIP).await.unwrap();
		assert_eq!(next(&mut import).await, first);
		let broadcast = origin.consume().request("demo", None).await.unwrap();
		let mut catalogs = hang::catalog::Catalog::<ts::Ext>::subscribe(&broadcast).await.unwrap();
		let catalog = catalogs.next().await.unwrap().expect("a catalog");
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
		assert_ne!(next(&mut import).await, first, "the new instance has its own epoch");
		let broadcast = origin.consume().request("demo", None).await.unwrap();
		let mut catalogs = hang::catalog::Catalog::<ts::Ext>::subscribe(&broadcast).await.unwrap();
		let catalog = catalogs.next().await.unwrap().expect("a catalog");
		assert!(catalog.echo.is_some(), "the new instance solicits feedback too");
		assert_eq!(
			catalog.ext.mpegts.mux_rate,
			Some(rate),
			"the new instance reuses the rate"
		);
		// Its first group is already out with the catalog.
		let (name, _) = catalog.video.renditions.iter().next().expect("a video rendition");
		let mut track = broadcast.track(name).unwrap().subscribe(None).await.unwrap();
		let group = track.recv_group().await.unwrap().expect("the first group");
		assert_eq!(group.sequence, 0, "the new instance's first group");
		drop(stdin);
		import.await.unwrap();
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
			let publish = Publish::stdin(&origin, "demo", &format, Default::default()).unwrap();
			let broadcast = publish.publication.clone().unwrap();
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
			let pending = Pending::new(broadcast, Default::default()).unwrap();
			let publication = Publication {
				pending: Some(pending),
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
		let broadcast = moq_net::broadcast::Info::new().produce();
		let mut publish = Publish::new(broadcast, &PublishFormat::Ts { program: None }, Default::default()).unwrap();
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
