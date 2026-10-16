use anyhow::Context;
use hang::moq_net;
use moq_mux::container::{flv, fmp4, ts};

/// Container format read from stdin on the import (source) side.
#[derive(Clone, Copy)]
pub enum PublishFormat {
	/// Raw AVC (H.264) Annex B elementary stream.
	Avc3,
	/// Fragmented MP4 (CMAF).
	Fmp4,
	/// MPEG-TS (transport stream), limited to one `program` of a multiplex when given.
	Ts { program: Option<u16> },
	/// MPEG-TS published whole, paced on `pcr_pid` when given rather than the PMT's clock.
	TsPassthrough { pcr_pid: Option<u16> },
	/// FLV (Flash Video / RTMP).
	Flv,
}

/// Command-line adapter for [`moq_video::encode::Codec`].
#[cfg(feature = "capture")]
#[derive(usage::ValueEnum, Clone, Copy, Default)]
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

/// Device capture options. Video (camera/screen -> H.264/H.265) maps to
/// `moq-video`; audio (microphone/system -> Opus) to `moq-audio`. Both are
/// captured by default; use `--no-video` / `--no-audio` to publish only one.
///
/// The video source is one of `--camera` / `--display` / `--window` / `--app`,
/// and the audio source one of `--microphone` / `--system-audio`, defaulting to
/// the default camera and microphone. Run `moq devices` to list the ids each one
/// takes.
#[cfg(feature = "capture")]
#[derive(usage::Args, Clone)]
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
	/// `--display` captures the main display. On Wayland the desktop portal opens
	/// a picker dialog; X11 accepts the listed monitor id.
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

	/// Capture audio only (no camera).
	#[usage(long, conflicts("--no-audio", "--camera", "--display", "--window", "--app"))]
	pub no_video: bool,

	/// Capture video only (no microphone).
	#[usage(long, conflicts("--microphone", "--system-audio"))]
	pub no_audio: bool,
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
	fn finish(&mut self) -> anyhow::Result<()> {
		match self {
			Self::Avc3 { split, import } => {
				let tail = split.flush(None)?;
				import.decode(tail)?;
				import.finish()?;
			}
			Self::Fmp4(d) => d.finish()?,
			Self::Ts(d) => d.finish()?,
			Self::TsPrograms(d) => d.finish()?,
			Self::TsPassthrough(d) => d.finish()?,
			Self::Flv(d) => d.finish()?,
		}
		Ok(())
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
) -> anyhow::Result<(ts::Import<ts::Ext>, moq_mux::catalog::Producer<ts::Ext>)> {
	let config = config.with_catalog(moq_mux::catalog::hang::Catalog::<ts::Ext>::default());
	let catalog = moq_mux::catalog::Producer::new(broadcast, config)?;
	let mut import = ts::Import::new(broadcast.clone(), catalog.reserve());
	if let Some(program) = program {
		import = import.with_program(program);
	}
	Ok((import, catalog))
}

// Exactly one Source exists per process, so the size gap between the small
// Stream variant and the larger Capture config is irrelevant.
#[allow(clippy::large_enum_variant)]
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
		video: Option<(moq_video::capture::Config, moq_video::encode::Options)>,
		audio: Option<(moq_audio::capture::Config, moq_audio::encode::Options)>,
	},
}

/// A stdin or capture publisher: decodes stdin (or captures local devices) into
/// a broadcast that the MoQ side announces.
pub struct Publish {
	source: Source,
	// Keeps the origin-created broadcast alive for the publisher's lifetime;
	// tracks and importers don't. `None` for `import ts --program all`, whose
	// programs each keep and announce their own.
	broadcast: Option<moq_net::broadcast::Producer>,
	// How long a finished stdin import stays up; see [`Self::linger`].
	linger: std::time::Duration,
	// A stdin import's route, announced once its catalog lists the tracks; see [`Self::announce`].
	route: Option<moq_net::origin::Route>,
	// The `--stats` report, written beside the decode loop; see [`Self::with_stats`].
	stats: Option<crate::report::Stats>,
}

/// A broadcast's announcement, held until its catalog first goes out.
///
/// The catalog withholds its first snapshot until the input names at least one servable track. A
/// relay queries a broadcast's tracks the moment it is announced, so a standby
/// announced any sooner refuses the tracks its epoch's live partner is serving, and the relay
/// keeps those refusals.
struct Pending {
	broadcast: moq_net::broadcast::Producer,
	catalog: moq_net::track::Consumer,
	route: moq_net::origin::Route,
}

impl Pending {
	fn new(broadcast: moq_net::broadcast::Producer, route: moq_net::origin::Route) -> anyhow::Result<Self> {
		let catalog = broadcast.consume().track(hang::Catalog::DEFAULT_NAME)?;
		Ok(Self {
			broadcast,
			catalog,
			route,
		})
	}

	/// Announce the broadcast if its catalog has gone out, returning whether it has.
	fn announce(&self) -> anyhow::Result<bool> {
		if self.catalog.latest().is_none() {
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
			let (ts, catalog) = ts_import(&mut broadcast, config, program)?;
			return Ok(Self {
				source: Source::Stream {
					decoder: PublishDecoder::Ts(Box::new(ts)),
					catalog: PublishCatalog::Ts(catalog),
				},
				broadcast: Some(broadcast),
				linger: Default::default(),
				route: None,
				stats: None,
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
			PublishFormat::Fmp4 => {
				let fmp4 = fmp4::Import::new(broadcast.clone(), catalog.reserve());
				PublishDecoder::Fmp4(Box::new(fmp4))
			}
			PublishFormat::Ts { .. } => unreachable!("TS is handled above with the mpegts catalog extension"),
			// The `m2ts` section is in the base catalog: nothing about the multiplex is described
			// beyond the track that carries it.
			PublishFormat::TsPassthrough { pcr_pid } => {
				let mut passthrough = ts::Passthrough::new(broadcast.clone(), catalog.reserve())?;
				if let Some(pid) = pcr_pid {
					passthrough = passthrough.with_pcr_pid(*pid);
				}
				PublishDecoder::TsPassthrough(Box::new(passthrough))
			}
			PublishFormat::Flv => {
				let flv = flv::Import::new(broadcast.clone(), catalog.reserve());
				PublishDecoder::Flv(Box::new(flv))
			}
		};

		Ok(Self {
			source: Source::Stream {
				decoder,
				catalog: PublishCatalog::Media(catalog),
			},
			broadcast: Some(broadcast),
			linger: Default::default(),
			route: None,
			stats: None,
		})
	}

	/// Build a publisher decoding a multi-program TS from stdin into one broadcast per program,
	/// named after `name` (`event.hang` becomes `event/1.hang`, `event/2.hang`, ...).
	///
	/// The programs aren't known until the PAT arrives, so each broadcast is created then and
	/// announced once its catalog has named a servable track, and [`announce`](Self::announce) has
	/// nothing to do.
	pub fn ts_programs(origin: moq_net::origin::Producer, name: String, config: moq_mux::catalog::Config) -> Self {
		Self {
			source: Source::Stream {
				decoder: PublishDecoder::TsPrograms(Box::new(ts::Programs::new(origin, name, config))),
				catalog: PublishCatalog::TsPrograms,
			},
			broadcast: None,
			linger: Default::default(),
			route: None,
			stats: None,
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
		mut broadcast: moq_net::broadcast::Producer,
		args: &CaptureArgs,
		bandwidth: moq_net::bandwidth::Allocator,
		max_age: Option<std::time::Duration>,
	) -> anyhow::Result<Self> {
		let config = moq_mux::catalog::Config::default().with_max_age(max_age);
		let catalog = moq_mux::catalog::Producer::new(&mut broadcast, config)?;

		let video = if args.no_video {
			None
		} else {
			Some((args.video_config()?, args.video_encode(bandwidth.clone())))
		};
		let audio = (!args.no_audio).then(|| (args.audio_config(), args.audio_encode(bandwidth)));
		anyhow::ensure!(video.is_some() || audio.is_some(), "nothing to capture");

		Ok(Self {
			source: Source::Capture {
				broadcast: broadcast.clone(),
				catalog,
				video,
				audio,
			},
			broadcast: Some(broadcast),
			linger: Default::default(),
			route: None,
			stats: None,
		})
	}

	/// Name a stats track in the catalog and write it beside the decode loop: what each track
	/// has written, and the transport of `connection` when there is one.
	///
	/// Call it before the importer declares its tracks, so the section rides the first catalog.
	pub fn with_stats(mut self, connection: Option<moq_tokio::connection::Monitor>) -> anyhow::Result<Self> {
		let (Some(broadcast), Source::Stream { catalog, .. }) = (self.broadcast.as_mut(), &self.source) else {
			anyhow::bail!("--stats reports a stdin import's one catalog");
		};
		let stats = match catalog {
			PublishCatalog::Media(catalog) => crate::report::Stats::new(broadcast, catalog, connection)?,
			PublishCatalog::Ts(catalog) => crate::report::Stats::new(broadcast, catalog, connection)?,
			PublishCatalog::TsPrograms => anyhow::bail!("`import ts --program all` builds a catalog per program"),
		};
		self.stats = Some(stats);
		Ok(self)
	}

	/// Keep a finished stdin import up for `linger` after EOF, so a subscriber that learns the
	/// final catalog late can still request and drain its tracks. Pass the media max-age: the
	/// content is servable until it goes stale. SIGINT or SIGTERM ends it early.
	pub fn linger(mut self, linger: std::time::Duration) -> Self {
		self.linger = linger;
		self
	}

	/// Advertise the broadcast's path under `epoch`.
	///
	/// A capture announces now. A stdin import announces from [`run`](Self::run), once the input
	/// has declared its tracks: see [`Pending`].
	pub fn announce(&mut self, epoch: moq_net::Epoch) -> anyhow::Result<()> {
		let route = moq_net::origin::Route::default().with_epoch(epoch);
		match (&self.source, &self.broadcast) {
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
				let pending = match (&self.broadcast, self.route) {
					(Some(broadcast), Some(route)) => Some(Pending::new(broadcast.clone(), route)?),
					_ => None,
				};
				decode(decoder, catalog, tokio::io::stdin(), self.linger, pending, self.stats).await
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
				// the wire. Video encodes on demand (camera opens only while
				// subscribed). Both run on this task rather than a spawn: on
				// macOS the audio future holds ObjC handles across an await,
				// so it is `!Send`.
				let video_fut = {
					let broadcast = broadcast.clone();
					let catalog = catalog.clone();
					async move {
						match video {
							Some((config, encode)) => {
								let mut options = moq_video::encode::Capture::default();
								options.capture = config;
								options.encode = encode;
								moq_video::encode::publish_capture(broadcast, catalog, options)
									.await
									.map_err(anyhow::Error::from)
							}
							None => Ok(()),
						}
					}
				};
				let audio_fut = async move {
					match audio {
						Some((config, encode)) => {
							let mut options = moq_audio::encode::Capture::default();
							options.capture = config;
							options.encode = encode;
							moq_audio::encode::publish_capture(broadcast, catalog, options)
								.await
								.map_err(anyhow::Error::from)
						}
						None => Ok(()),
					}
				};

				tokio::try_join!(video_fut, audio_fut)?;
				Ok(())
			}
		}
	}
}

/// Decode `input` into the broadcast until EOF.
///
/// At EOF the media tracks finish, then the catalog does, while it still lists them: the
/// renditions retire from the catalog as the decoder drops, which a subscriber would read
/// as removed tracks, and a catalog dropped unfinished reads as a publisher that vanished.
/// The finished tracks then stay servable for `linger`.
///
/// `stats` reports beside the read loop, so a stalled input keeps reporting, and ends with its
/// final counts when the media tracks do.
async fn decode(
	mut decoder: PublishDecoder,
	mut catalog: PublishCatalog,
	mut input: impl tokio::io::AsyncRead + Unpin,
	linger: std::time::Duration,
	mut pending: Option<Pending>,
	mut stats: Option<crate::report::Stats>,
) -> anyhow::Result<()> {
	// An empty BytesMut grows to only 64 bytes and clearing it keeps that capacity.
	// Stdin dispatches every read to a blocking thread, so those tiny reads can
	// backpressure a live feed. Reserve a full IO chunk; read_buf still returns
	// whatever is available without waiting to fill it.
	let mut buffer = bytes::BytesMut::with_capacity(64 * 1024);

	// Counters reported so far, so only the change is logged. A live feed is
	// diagnosed by the rate at which these climb, and stdin may never end, so
	// they have to surface as they accumulate rather than at exit.
	let mut log = ts::stats::Log::default();
	let mut sampled = tokio::time::Instant::now();

	// Run the read/decode loop so an error surfaces here rather than
	// dropping the decoder (and its tracks) with a bare Error::Dropped.
	let read = async {
		loop {
			buffer.clear();
			let n = tokio::io::AsyncReadExt::read_buf(&mut input, &mut buffer).await?;
			if n == 0 {
				return Ok(()); // EOF
			}
			decoder.decode_chunk(&buffer)?;
			if let Some(announce) = &pending
				&& announce.announce()?
			{
				pending = None;
			}

			if sampled.elapsed() >= ts::stats::Log::INTERVAL {
				sampled = tokio::time::Instant::now();
				if let Some(stats) = decoder.stats() {
					log.sample(stats);
				}
			}
		}
	};
	let result: anyhow::Result<()> = match stats.as_mut() {
		Some(stats) => tokio::select! {
			result = read => result,
			never = stats.run() => match never {},
		},
		None => read.await,
	};

	// Flush on a clean EOF; on any error (read, decode, or the flush
	// itself) abort with the real cause so subscribers see it instead of
	// a bare Error::Dropped.
	let outcome = result.and_then(|()| decoder.finish());
	// The drain can release the first catalog. Without one the input declared no track, and
	// announcing would only advertise an empty broadcast.
	let outcome = outcome.and_then(|()| match &pending {
		Some(announce) if !announce.announce()? => anyhow::bail!("input ended before it declared a track"),
		_ => Ok(()),
	});
	// The drain at end of input can publish a frame nothing vouched for, so the
	// final snapshot is only complete after `finish`.
	if let Some(stats) = decoder.stats() {
		log.finish(&stats);
	}
	match outcome {
		Ok(()) => {
			if let Some(stats) = &mut stats {
				stats.finish();
			}
			catalog.finish()?;
			// The decoder, catalog, and stats report own the tracks, so holding them keeps the
			// finished broadcast servable: a subscriber that saw the final catalog only at EOF
			// still has to request its media.
			tracing::info!(
				?linger,
				"input ended; serving the finished broadcast until its media is stale"
			);
			tokio::time::sleep(linger).await;
			Ok(())
		}
		Err(err) => {
			let reason = moq_net::Error::Transport(err.to_string());
			catalog.abort(reason.clone());
			decoder.abort(reason);
			Err(err)
		}
	}
}

#[cfg(feature = "capture")]
impl CaptureArgs {
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
		} else if let Some(index) = &self.display {
			Source::Display(index.clone())
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
			.map(|fps| moq_video::Rate::new(fps, 1))
			.transpose()
			.map_err(anyhow::Error::from)?;
		config.cursor = !self.no_cursor;
		Ok(config)
	}

	fn video_encode(&self, bandwidth: moq_net::bandwidth::Allocator) -> moq_video::encode::Options {
		let mut options = moq_video::encode::Options::default();
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
		options
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

	/// The audio counterpart to [`video_encode`](Self::video_encode). `track` is
	/// left unset so the name derives from the codec, the way the video side
	/// names its track; consumers find it through the catalog either way.
	fn audio_encode(&self, bandwidth: moq_net::bandwidth::Allocator) -> moq_audio::encode::Options {
		let mut options = moq_audio::encode::Options::default();
		options.settings.bitrate = self
			.audio_bitrate
			.map(|bps| moq_net::bandwidth::Rate::from_bps(bps.into()));
		options.bandwidth = bandwidth;
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
		let args = capture_args(&["--window", "--no-audio"]);
		assert!(args.no_audio);
		assert_eq!(args.window, Some(None));
		assert_eq!(capture_args(&["--window"]).window, Some(None));
	}

	#[cfg(feature = "capture")]
	#[test]
	fn capture_window_id_preserves_native_selection() {
		for id in ["39193", "x11:0x400001"] {
			let config = capture_args(&["--window", id, "--no-audio"]).video_config().unwrap();
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
		let config = capture_args(&["--window", "--no-audio"]).video_config().unwrap();
		assert!(matches!(config.source, moq_video::capture::Source::Portal(_)));
	}

	#[cfg(all(feature = "capture", not(all(target_os = "linux", feature = "pipewire"))))]
	#[test]
	fn capture_window_picker_refuses_unsupported_builds() {
		let error = capture_args(&["--window", "--no-audio"]).video_config().unwrap_err();
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

	/// Payload of an undecoded PES-framed stream (e.g. teletext/DVB AC-3 private data),
	/// carried verbatim on its own PID with the original PES stream_id.
	const PES_PAYLOAD: &[u8] = &[0xDE, 0xAD, 0xBE, 0xEF, 0x01, 0x02];

	const SECTION_PID: u16 = 0x102;
	const VERBATIM_PES_PID: u16 = 0x104;
	const VERBATIM_PES_STREAM_ID: u8 = 0xC0;

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
		while let Ok(res) = tokio::time::timeout(wait, exporter.next()).await {
			match res.expect("exporter error") {
				Some(frame) => out.extend_from_slice(&frame.payload),
				None => break,
			}
		}
		out
	}

	/// Manufacture a TS feed carrying real video/audio plus one section-framed
	/// verbatim stream (SCTE-35) and one PES-framed verbatim stream, by importing
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
		let mut broadcast = origin.create_broadcast("cli").unwrap();
		broadcast.announce(Default::default()).unwrap();
		settle().await;
		let config = moq_mux::catalog::Config::default().with_catalog(Catalog::<tscat::Ext>::default());
		let mut catalog = moq_mux::catalog::Producer::new(&mut broadcast, config).unwrap();
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

		// PES-framed verbatim stream (undecoded private data, stream_type 0x06), with
		// an explicit PES stream_id to round-trip.
		let pes = broadcast
			.unique_track(".data", hang::container::track_info(hang::catalog::PRIORITY.text))
			.unwrap();
		let mut verbatim = tscat::Verbatim::new(0x06, tscat::Framing::Pes);
		verbatim.stream_id = Some(VERBATIM_PES_STREAM_ID);
		let mut pes_track = tscat::Track::new(VERBATIM_PES_PID);
		pes_track.verbatim = Some(verbatim);
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
				payload: bytes::Bytes::from_static(PES_PAYLOAD),
				keyframe: true,
			})
			.unwrap();
		pes_producer.cut(None).unwrap();
		pes_producer.finish().unwrap();

		// Add the real video/audio (moves `broadcast` into the importer).
		let mut import = Import::new(broadcast, reserved);
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
		let broadcast = consumer.request_broadcast(path, None).await.unwrap();
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
	/// and the SCTE-35 section and the verbatim PES survive with their PIDs, framing,
	/// PES stream_id, and byte-exact payloads.
	async fn ts_verbatim_round_trip(format: CatalogFormat) {
		// Paused time auto-advances when the exporter parks, so the `drain` timeouts
		// fire instantly instead of waiting on the wall clock.
		let input = manufacture_input().await;

		// Publish side: `Publish::new(Ts)` builds a `ts::Import<Ext>`, so the verbatim
		// streams land in the broadcast instead of being dropped by the media-only path.
		// The broadcast is created on a throwaway origin so the exporter can resolve it by path.
		let origin = moq_tokio::origin::spawn();
		let broadcast = origin.create_broadcast("cli").unwrap();
		broadcast.announce(Default::default()).unwrap();
		settle().await;
		let mut publish = Publish::new(broadcast, &PublishFormat::Ts { program: None }, Default::default()).unwrap();
		#[allow(irrefutable_let_patterns)]
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
			.find(|(_, t)| t.verbatim.as_ref().is_some_and(|v| v.stream_type == 0x06))
			.expect("verbatim PES survived the round-trip");
		assert_eq!(pes.pid, VERBATIM_PES_PID, "verbatim PES PID preserved");
		let pes_verbatim = pes.verbatim.as_ref().unwrap();
		assert_eq!(pes_verbatim.framing, tscat::Framing::Pes, "PES framing preserved");
		assert_eq!(
			pes_verbatim.stream_id,
			Some(VERBATIM_PES_STREAM_ID),
			"PES stream_id preserved"
		);
		let pes_name = pes_name.clone();

		assert_eq!(
			read_frame(&consumer, &section_name).await,
			CUE,
			"SCTE-35 section round-trips byte-for-byte"
		);
		assert_eq!(
			read_frame(&consumer, &pes_name).await,
			PES_PAYLOAD,
			"verbatim PES payload round-trips byte-for-byte"
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
		#[allow(irrefutable_let_patterns)]
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
	/// a flagged PCR jump twenty seconds forward. The jump crosses as one skipped sequence, which
	/// neither the relay nor a late subscriber waits on, and the timestamps carry it, so a budget
	/// shorter than the jump drops the groups before it.
	#[tokio::test]
	async fn ts_passthrough_crosses_a_relay_through_a_flagged_jump() {
		const KYRION: &[u8] = include_bytes!("../../moq-mux/src/container/ts/test_data/kyrion_mpeg2av_ac3.ts");
		// The capture twice, the second copy's clock twenty seconds ahead of the first, its first
		// PCR flagged as the discontinuity. Inside the track's max age, so the recording keeps
		// every group.
		const STEP: u64 = 20 * 90_000;
		let mut second = KYRION.to_vec();
		let mut pcrs = second
			.as_chunks_mut::<188>()
			.0
			.iter_mut()
			.filter(|pkt| pkt[3] & 0x20 != 0 && pkt[4] > 0 && pkt[5] & 0x10 != 0)
			.peekable();
		pcrs.peek_mut().expect("a PCR")[5] |= 0x80;
		for pkt in pcrs {
			let base = pkt[6..11].iter().fold(0u64, |base, byte| base << 8 | u64::from(*byte)) >> 7;
			let base = ((base + STEP) & ((1 << 33) - 1)) << 7 | u64::from(pkt[10] & 0x7f);
			pkt[6..11].copy_from_slice(&base.to_be_bytes()[3..]);
		}
		let input = [KYRION, &second].concat();

		let _env = crate::test_env::EnvGuard::clear(&["MOQ_CONNECT", "MOQ_HOP"]);
		let _ = moq_tokio::crypto::install_default();
		for version in ["moq-lite-06", "moq-transport-14"] {
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
			let broadcast = origin.create_broadcast("pt.hang").unwrap();
			broadcast.announce(Default::default()).unwrap();
			let mut publish = Publish::new(
				broadcast,
				&PublishFormat::TsPassthrough { pcr_pid: None },
				Default::default(),
			)
			.unwrap();
			#[allow(irrefutable_let_patterns)]
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

			// The catalog names the track once the first group is out, which the first copy holds;
			// the jump is in the second.
			const WAIT: Duration = Duration::from_secs(10);
			decoder.decode_chunk(KYRION).unwrap();
			let consumer = tokio::time::timeout(WAIT, remote.consume().routed_broadcast("pt.hang"))
				.await
				.unwrap()
				.unwrap();
			let mut catalogs = hang::catalog::Catalog::<()>::subscribe(&consumer).await.unwrap();
			let catalog = tokio::time::timeout(WAIT, catalogs.next())
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
				let first = tokio::time::timeout(WAIT, track.recv_group()).await.unwrap().unwrap();
				(track, first)
			};
			// Each group's sequence and frames, read until the track ends. A read cut off as
			// stale leaves `None` for that group's frames.
			let read = |(mut track, first): (moq_net::track::Subscriber, Option<moq_net::group::Consumer>)| {
				tokio::spawn(tokio::time::timeout(WAIT, async move {
					let mut groups = Vec::new();
					let mut next = first;
					while let Some(mut group) = next {
						let mut frames = Some(Vec::new());
						loop {
							match group.read_frame().await {
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
			let first = |frames: &[hang::container::Frame]| frames[0].timestamp;
			let jump = recording
				.windows(2)
				.find(|pair| first(&pair[1].1).as_micros() > first(&pair[0].1).as_micros() + 10_000_000)
				.map(|pair| pair[1].0)
				.unwrap_or_else(|| panic!("{version}: no jump crossed"));
			let last = recording.last().unwrap().0;
			assert_eq!(
				recording.iter().map(|(sequence, _)| *sequence).collect::<Vec<_>>(),
				(recording[0].0..jump - 1).chain(jump..=last).collect::<Vec<_>>(),
				"{version}: no group missing but the break marker"
			);

			// Joining once every group is out, with a budget half the jump, reads every group
			// after it past the skipped sequence, and none of the groups the jump left more than
			// the budget behind. An IETF subscription starts at the newest group, so only lite
			// has the groups behind it to read.
			if version.starts_with("moq-lite") {
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
		#[allow(irrefutable_let_patterns)]
		let Source::Stream { decoder, .. } = &mut publish.source else {
			panic!("expected a stream source");
		};
		let before = std::time::SystemTime::now();
		decoder.decode_chunk(BBB).unwrap();
		let after = std::time::SystemTime::now();
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
		#[allow(irrefutable_let_patterns)]
		let Source::Stream { decoder, catalog } = publish.source else {
			panic!("expected a stream source");
		};
		let mut input = Packets {
			remaining: BBB,
			reads: 0,
		};
		decode(decoder, catalog, &mut input, Duration::ZERO, None, None)
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
			#[allow(irrefutable_let_patterns)]
			let Source::Stream { decoder, catalog } = publish.source else {
				panic!("expected a stream source");
			};
			let start = std::time::Instant::now();
			decode(decoder, catalog, input, Duration::ZERO, None, None)
				.await
				.unwrap();
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

		#[allow(irrefutable_let_patterns)]
		let Source::Stream { decoder, catalog } = publish.source else {
			panic!("expected a stream source");
		};
		decode(decoder, catalog, BBB, Duration::ZERO, None, None).await.unwrap();
		drop(publish.broadcast);

		let mut last = None;
		loop {
			let next = tokio::time::timeout(Duration::from_secs(1), catalogs.next())
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
	/// the final catalog only then still gets the media, and the import returns once it ends.
	#[tokio::test(start_paused = true)]
	async fn a_finished_import_serves_its_tracks_for_the_linger() {
		const LINGER: Duration = Duration::from_secs(30);
		let broadcast = moq_net::broadcast::Info::new().produce();
		let consumer = broadcast.consume();
		let publish = Publish::new(broadcast, &PublishFormat::Ts { program: None }, Default::default()).unwrap();
		let mut catalogs = hang::catalog::Catalog::<()>::subscribe(&consumer).await.unwrap();

		#[allow(irrefutable_let_patterns)]
		let Source::Stream { decoder, catalog } = publish.source else {
			panic!("expected a stream source");
		};
		let started = tokio::time::Instant::now();
		let mut import = std::pin::pin!(decode(decoder, catalog, BBB, LINGER, None, None));
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
		drop(publish.broadcast);
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
			(PublishFormat::Fmp4, FMP4),
		] {
			let origin = moq_tokio::origin::spawn();
			let consumer = origin.consume();
			let mut announced = consumer.announced();
			let broadcast = origin.create_broadcast("demo").unwrap();
			let publish = Publish::new(broadcast.clone(), &format, Default::default()).unwrap();
			#[allow(irrefutable_let_patterns)]
			let Source::Stream { decoder, catalog } = publish.source else {
				panic!("expected a stream source");
			};
			let route = moq_net::origin::Route::default().with_epoch(moq_net::Epoch::mint());
			let pending = Pending::new(broadcast, route).unwrap();
			let (mut stdin, input_end) = tokio::io::duplex(input.len());
			let mut import = std::pin::pin!(decode(decoder, catalog, input_end, Duration::ZERO, Some(pending), None));

			tokio::select! {
				res = &mut import => panic!("the import returned before its input ended: {res:?}"),
				event = tokio::time::timeout(Duration::from_secs(5), announced.next()) => {
					assert!(event.is_err(), "announced before any input: {event:?}");
				}
			}

			tokio::io::AsyncWriteExt::write_all(&mut stdin, input).await.unwrap();
			let resolved = tokio::select! {
				res = &mut import => panic!("the import returned before its input ended: {res:?}"),
				_ = tokio::time::sleep(Duration::from_secs(30)) => panic!("never announced"),
				resolved = async {
					let event = announced.next().await.expect("origin closed");
					assert!(matches!(event, moq_net::announce::Event::Start(_)), "{event:?}");
					let broadcast = consumer.request_broadcast("demo", None).await.unwrap();
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
		#[allow(irrefutable_let_patterns)]
		let Source::Stream { decoder, catalog } = publish.source else {
			panic!("stream source");
		};
		let err = decode(decoder, catalog, Broken, Duration::ZERO, None, None)
			.await
			.unwrap_err();
		assert_eq!(err.to_string(), "stdin failed");
		assert!(
			matches!(catalog_reader.recv_group().await, Err(moq_net::Error::Transport(ref reason)) if reason == "stdin failed")
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
			let broadcast = origin.create_broadcast("demo").unwrap();
			let publish = Publish::new(broadcast.clone(), &format, Default::default()).unwrap();
			#[allow(irrefutable_let_patterns)]
			let Source::Stream { decoder, catalog } = publish.source else {
				panic!("expected a stream source");
			};
			let pending = Pending::new(broadcast, Default::default()).unwrap();
			let err = decode(decoder, catalog, &input[..], Duration::ZERO, Some(pending), None)
				.await
				.unwrap_err();
			assert!(err.to_string().contains("declared"), "{err:#}");
			let event = tokio::time::timeout(Duration::from_secs(1), announced.next()).await;
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
		#[allow(irrefutable_let_patterns)]
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
		let frame = tokio::time::timeout(Duration::from_secs(1), next_frame(&mut reader))
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
