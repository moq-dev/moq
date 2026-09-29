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

	/// Capture a single window, by the id `moq devices` reports. Supported on
	/// macOS, Windows, and X11.
	#[usage(long, group = "video-source")]
	pub window: Option<String>,

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
	TsPrograms(Box<TsPrograms>),
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
			Self::Flv(d) => d.decode(chunk)?,
		}
		Ok(())
	}

	/// What each elementary stream has delivered and the audio frame sync lost so far, for
	/// the formats that report it.
	fn stats(&self) -> Option<ts::Stats> {
		match self {
			Self::Ts(d) => Some(d.stats()),
			Self::TsPrograms(d) => Some(d.stats()),
			Self::Avc3 { .. } | Self::Fmp4(_) | Self::Flv(_) => None,
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
	/// `import ts --program all`: each program's catalog, finished by [`TsPrograms::finish`].
	TsPrograms,
}

impl PublishCatalog {
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
	let mut import = ts::Import::new(broadcast.clone(), catalog.reserve()).live();
	if let Some(program) = program {
		import = import.with_program(program);
	}
	Ok((import, catalog))
}

/// One program's broadcast under `import ts --program all`.
struct ProgramImport {
	/// Keeps the origin-created broadcast alive; the importer's clone doesn't.
	_broadcast: moq_net::broadcast::Producer,
	import: ts::Import<ts::Ext>,
	catalog: moq_mux::catalog::Producer<ts::Ext>,
}

/// `import ts --program all`: every program the first PAT lists, each published as its own
/// broadcast on its own clock and catalog.
///
/// Input ahead of that PAT is dropped, as a single importer drops media ahead of its PSI. A
/// program a later PAT adds is not published; one it removes ends the import.
struct TsPrograms {
	origin: moq_net::origin::Producer,
	name: String,
	config: moq_mux::catalog::Config,
	/// Input held until it contains a whole PAT.
	pending: Vec<u8>,
	/// Empty until the PAT arrives.
	programs: Vec<ProgramImport>,
}

impl TsPrograms {
	fn new(origin: moq_net::origin::Producer, name: String, config: moq_mux::catalog::Config) -> Self {
		Self {
			origin,
			name,
			config,
			pending: Vec::new(),
			programs: Vec::new(),
		}
	}

	fn decode(&mut self, chunk: &[u8]) -> anyhow::Result<()> {
		if !self.programs.is_empty() {
			return self.feed(chunk);
		}
		self.pending.extend_from_slice(chunk);
		let Some(programs) = ts::programs(&self.pending).filter(|programs| !programs.is_empty()) else {
			// Only a packet's worth of tail can still hold the start of the PAT.
			let keep = self.pending.len().saturating_sub(TS_PACKET_SIZE - 1);
			self.pending.drain(..keep);
			return Ok(());
		};
		for program in programs {
			let name = program_broadcast(&self.name, program);
			let mut broadcast = self
				.origin
				.create_broadcast(&name)
				.with_context(|| format!("failed to create broadcast {name}"))?;
			let (import, catalog) = ts_import(&mut broadcast, self.config.clone(), Some(program))?;
			broadcast
				.announce(Default::default())
				.with_context(|| format!("failed to announce broadcast {name}"))?;
			self.programs.push(ProgramImport {
				_broadcast: broadcast,
				import,
				catalog,
			});
		}
		let pending = std::mem::take(&mut self.pending);
		self.feed(&pending)
	}

	fn feed(&mut self, chunk: &[u8]) -> anyhow::Result<()> {
		for program in &mut self.programs {
			program.import.decode(chunk)?;
		}
		Ok(())
	}

	/// Every program's counters in one map: PIDs are unique across a multiplex.
	fn stats(&self) -> ts::Stats {
		let mut stats = ts::Stats::default();
		for program in &self.programs {
			stats.streams.extend(program.import.stats().streams);
		}
		stats
	}

	/// Finish each program's tracks, then its catalog while it still lists them.
	fn finish(&mut self) -> anyhow::Result<()> {
		for program in &mut self.programs {
			program.import.finish()?;
			program.catalog.finish()?;
		}
		Ok(())
	}

	fn abort(self, err: moq_net::Error) {
		for program in self.programs {
			program.import.abort(err.clone());
		}
	}
}

const TS_PACKET_SIZE: usize = 188;

/// The broadcast one program of `name` publishes on, keeping the catalog suffix last so format
/// detection still sees it: `event.hang` becomes `event/2.hang`.
fn program_broadcast(name: &str, program: u16) -> String {
	match moq_mux::catalog::CatalogFormat::detect(name) {
		Some(format) => {
			let stem = &name[..name.len() - format.extension().len()];
			format!("{stem}/{program}{}", format.extension())
		}
		None => format!("{name}/{program}"),
	}
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
}

impl Publish {
	/// Build a publisher decoding the given container format from stdin into
	/// `broadcast`. Announce the broadcast afterwards: this constructor creates
	/// the catalog tracks, so announcing after it lands the advertisement with
	/// the tracks already in place.
	///
	/// Stdin is a live feed with its own zero, so the container importers translate its
	/// timestamps onto the broadcast clock the catalog advertises (`live`).
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
				let fmp4 = fmp4::Import::new(broadcast.clone(), catalog.reserve()).live();
				PublishDecoder::Fmp4(Box::new(fmp4))
			}
			PublishFormat::Ts { .. } => unreachable!("TS is handled above with the mpegts catalog extension"),
			PublishFormat::Flv => {
				let flv = flv::Import::new(broadcast.clone(), catalog.reserve()).live();
				PublishDecoder::Flv(Box::new(flv))
			}
		};

		Ok(Self {
			source: Source::Stream {
				decoder,
				catalog: PublishCatalog::Media(catalog),
			},
			broadcast: Some(broadcast),
		})
	}

	/// Build a publisher decoding a multi-program TS from stdin into one broadcast per program,
	/// named after `name` (`event.hang` becomes `event/1.hang`, `event/2.hang`, ...).
	///
	/// The programs aren't known until the PAT arrives, so each broadcast is created and
	/// announced then, and [`announce`](Self::announce) has nothing to do.
	pub fn ts_programs(origin: moq_net::origin::Producer, name: String, config: moq_mux::catalog::Config) -> Self {
		Self {
			source: Source::Stream {
				decoder: PublishDecoder::TsPrograms(Box::new(TsPrograms::new(origin, name, config))),
				catalog: PublishCatalog::TsPrograms,
			},
			broadcast: None,
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
		})
	}

	/// Advertise the broadcast's path, now that the catalog tracks are in place.
	pub fn announce(&self) -> anyhow::Result<()> {
		let Some(broadcast) = &self.broadcast else {
			return Ok(());
		};
		broadcast
			.announce(Default::default())
			.context("failed to announce broadcast")
	}

	/// Drive the source until stdin EOF (or the capture devices stop).
	pub async fn run(self) -> anyhow::Result<()> {
		match self.source {
			Source::Stream { decoder, catalog } => decode(decoder, catalog, tokio::io::stdin()).await,
			#[cfg(feature = "capture")]
			Source::Capture {
				broadcast,
				catalog,
				video,
				audio,
			} => {
				// Each enabled medium publishes its own track onto the shared
				// broadcast + catalog. Frames are stamped from the catalog's
				// advertised clock so HLS/DASH wall times match the mapping on
				// the wire. Video encodes on demand (camera opens only while
				// subscribed). Both run on this task rather than a spawn: on
				// macOS the audio future holds ObjC handles across an await,
				// so it is `!Send`.
				let clock = catalog.clock();
				let video_fut = {
					let broadcast = broadcast.clone();
					let catalog = catalog.clone();
					async move {
						match video {
							Some((config, encode)) => {
								moq_video::encode::publish_capture(broadcast, catalog, config, encode, clock)
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
							let mut options = moq_audio::encode::PublicationOptions::default();
							options.capture = config;
							options.encode = encode;
							options.clock = clock;
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
async fn decode(
	mut decoder: PublishDecoder,
	mut catalog: PublishCatalog,
	mut input: impl tokio::io::AsyncRead + Unpin,
) -> anyhow::Result<()> {
	let mut buffer = bytes::BytesMut::new();

	// Counters reported so far, so only the change is logged. A live feed is
	// diagnosed by the rate at which these climb, and stdin may never end, so
	// they have to surface as they accumulate rather than at exit.
	let mut log = ts::stats::Log::default();
	let mut sampled = tokio::time::Instant::now();

	// Run the read/decode loop so an error surfaces here rather than
	// dropping the decoder (and its tracks) with a bare Error::Dropped.
	let result: anyhow::Result<()> = async {
		loop {
			buffer.clear();
			let n = tokio::io::AsyncReadExt::read_buf(&mut input, &mut buffer).await?;
			if n == 0 {
				return Ok(()); // EOF
			}
			decoder.decode_chunk(&buffer)?;

			if sampled.elapsed() >= ts::stats::Log::INTERVAL {
				sampled = tokio::time::Instant::now();
				if let Some(stats) = decoder.stats() {
					log.sample(stats);
				}
			}
		}
	}
	.await;

	// Flush on a clean EOF; on any error (read, decode, or the flush
	// itself) abort with the real cause so subscribers see it instead of
	// a bare Error::Dropped.
	let outcome = result.and_then(|()| decoder.finish());
	// The drain at end of input can publish a frame nothing vouched for, so the
	// final snapshot is only complete after `finish`.
	if let Some(stats) = decoder.stats() {
		log.finish(&stats);
	}
	match outcome {
		Ok(()) => catalog.finish(),
		Err(err) => {
			decoder.abort(moq_net::Error::Transport(err.to_string()));
			Err(err)
		}
	}
}

#[cfg(feature = "capture")]
impl CaptureArgs {
	/// The video source named by the flags, defaulting to the default camera.
	/// The `video-source` arg group makes these mutually exclusive, so the order
	/// here only decides the default.
	fn video_source(&self) -> moq_video::capture::Source {
		use moq_video::capture::Source;

		if let Some(id) = &self.window {
			Source::Window(id.clone())
		} else if let Some(id) = &self.app {
			Source::App(id.clone())
		} else if let Some(index) = &self.display {
			Source::Display(index.clone())
		} else {
			Source::Camera(self.camera.clone().flatten())
		}
	}

	fn video_config(&self) -> anyhow::Result<moq_video::capture::Config> {
		let mut config = moq_video::capture::Config::default();
		config.source = self.video_source();
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
mod tests {
	use std::time::Duration;

	use bytes::BytesMut;
	use moq_mux::catalog::CatalogFormat;
	use moq_mux::catalog::hang::{Catalog, Container};
	use moq_mux::container::ts::{self as tscat, Export, Import};
	use moq_mux::container::{Consumer, Frame, Producer};
	use moq_net::Timestamp;

	use super::*;

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

	/// Drain an exporter, concatenating every frame's payload until output stops. The
	/// producers stay alive (retained tracks), so the stream never hard-ends; pull until a
	/// `next()` blocks, surfaced here as a timeout once the buffered frames are gone.
	async fn drain(mut exporter: Export<tscat::Ext>) -> Vec<u8> {
		let mut out = Vec::new();
		while let Ok(res) = tokio::time::timeout(Duration::from_millis(500), exporter.next()).await {
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
		// bbb's first video keyframe is at 1.4 s; stamp the ancillary streams just after
		// it so they clear the export's keyframe alignment (anything before the first
		// keyframe is dropped on tune-in).
		section_producer
			.write(Frame {
				timestamp: Timestamp::from_millis(1410).unwrap(),
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
				timestamp: Timestamp::from_millis(1410).unwrap(),
				duration: None,
				payload: bytes::Bytes::from_static(PES_PAYLOAD),
				keyframe: true,
			})
			.unwrap();
		pes_producer.cut(None).unwrap();
		pes_producer.finish().unwrap();

		// Add the real video/audio (moves `broadcast` into the importer).
		let mut import = Import::new(broadcast, catalog.reserve());
		import.decode(&BytesMut::from(BBB)).unwrap();
		import.finish().unwrap();

		// `catalog`, the producers, and `import` stay alive: the exporter subscribes to
		// the retained tracks.
		drain(
			Export::with_ts(moq_mux::Source::new(origin.consume(), "cli"), CatalogFormat::Hang)
				.await
				.unwrap()
				.with_max_age(RECORDING_MAX_AGE),
		)
		.await
	}

	/// The media track's full retention window, so an exporter started after publishing
	/// can still read every retained group. These tests publish a whole feed before
	/// exporting it, which the default
	/// [`Duration::ZERO`] collapses to the live edge:
	/// completeness has to be asked for, exactly as a real recorder does.
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
	/// (which selects the `mpegts` catalog) and the subscribe-side `Export::with_ts`,
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

		// Subscribe side: the same `with_ts` call `run_ts` makes, re-emitting the
		// ancillary streams verbatim.
		let output = drain(
			Export::with_ts(moq_mux::Source::new(origin.consume(), "cli"), format)
				.await
				.unwrap()
				.with_max_age(RECORDING_MAX_AGE),
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

	/// `moq import ts` publishes on the broadcast clock it advertises: a feed arriving a minute
	/// after the broadcast began is live on arrival, not stamped with its own PTS (1.4s into bbb).
	#[tokio::test]
	async fn ts_import_publishes_on_the_broadcast_clock() {
		let ago = Duration::from_secs(60);
		let clock = moq_mux::Clock::at(std::time::Instant::now() - ago, std::time::SystemTime::now() - ago).unwrap();
		let broadcast = moq_net::broadcast::Info::new().produce();
		let consumer = broadcast.consume();
		let config = moq_mux::catalog::Config::default().with_clock(clock);
		let mut publish = Publish::new(broadcast, &PublishFormat::Ts { program: None }, config).unwrap();
		#[allow(irrefutable_let_patterns)]
		let Source::Stream { decoder, .. } = &mut publish.source else {
			panic!("expected a stream source");
		};
		let before = clock.now();
		decoder.decode_chunk(BBB).unwrap();
		let after = clock.now();
		decoder.finish().unwrap();

		let catalog = hang::catalog::Catalog::<()>::subscribe(&consumer)
			.await
			.unwrap()
			.next()
			.await
			.unwrap()
			.expect("a catalog");
		assert_eq!(
			catalog.clock,
			Some(clock.wall()),
			"the advertised clock is the one stamped on"
		);
		let (name, config) = catalog.video.renditions.iter().next().expect("a video rendition");
		let track = consumer.track(name).unwrap().subscribe(None).await.unwrap();
		let container = moq_mux::catalog::hang::Container::try_from(config).unwrap();
		let first = Consumer::new(track, container)
			.read()
			.await
			.unwrap()
			.expect("a video frame")
			.timestamp;
		// The PES that anchors the mapping need not be this frame: the mux spaces them apart.
		let skew = Duration::from_secs(2).as_micros();
		assert!(
			before.as_micros() - skew <= first.as_micros() && first.as_micros() <= after.as_micros() + skew,
			"the first frame is live on arrival: {first:?} not in {before:?}..={after:?}"
		);
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
		decode(decoder, catalog, BBB).await.unwrap();
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

	#[test]
	fn program_broadcasts_keep_the_catalog_suffix_last() {
		assert_eq!(program_broadcast("event.hang", 2), "event/2.hang");
		assert_eq!(program_broadcast("demo/event.msf", 7), "demo/event/7.msf");
		assert_eq!(program_broadcast("event", 1), "event/1");
	}

	/// A PAT listing two programs, then one MP2 PES of each: program 1 on PID `0x61` at 1 s
	/// with fill bytes `0xAA`/`0xBB`, program 2 on PID `0x71` an hour later with `0xCC`/`0xDD`.
	fn two_programs() -> Vec<u8> {
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

	/// `import ts --program all` holds the input until the PAT is whole, then publishes each
	/// program as its own broadcast carrying only that program, live on its own first frame
	/// rather than an hour apart on one shared clock.
	#[tokio::test]
	async fn every_program_publishes_its_own_broadcast() {
		let origin = moq_tokio::origin::spawn();
		let ago = Duration::from_secs(60);
		let clock = moq_mux::Clock::at(std::time::Instant::now() - ago, std::time::SystemTime::now() - ago).unwrap();
		let config = moq_mux::catalog::Config::default().with_clock(clock);
		let publish = Publish::ts_programs(origin.clone(), "event.hang".to_string(), config);
		let Source::Stream {
			decoder: PublishDecoder::TsPrograms(mut programs),
			..
		} = publish.source
		else {
			panic!("expected the per-program decoder");
		};

		let input = two_programs();
		let before = clock.now();
		programs.decode(&input[..100]).unwrap();
		assert!(
			programs.programs.is_empty(),
			"no program is known before the PAT is whole"
		);
		programs.decode(&input[100..]).unwrap();
		programs.finish().unwrap();
		let after = clock.now();

		for (path, fills) in [("event/1.hang", [0xAA, 0xBB]), ("event/2.hang", [0xCC, 0xDD])] {
			let consumer = moq_mux::Source::new(origin.consume(), path).broadcast().await.unwrap();
			let catalog = hang::catalog::Catalog::<()>::subscribe(&consumer)
				.await
				.unwrap()
				.next()
				.await
				.unwrap()
				.expect("a catalog");
			let renditions: Vec<_> = catalog.audio.renditions.iter().collect();
			assert_eq!(renditions.len(), 1, "{path} carries its own program's one stream");
			let (name, config) = renditions[0];
			let track = consumer.track(name).unwrap().subscribe(None).await.unwrap();
			let container = Container::try_from(config).unwrap();
			let frame = Consumer::new(track, container).read().await.unwrap().expect("a frame");
			assert!(fills.contains(&frame.payload[4]), "{path} carries only its own program");
			let skew = Duration::from_secs(2).as_micros();
			assert!(
				before.as_micros() - skew <= frame.timestamp.as_micros()
					&& frame.timestamp.as_micros() <= after.as_micros() + skew,
				"{path} is live on arrival: {:?} not in {before:?}..={after:?}",
				frame.timestamp
			);
		}
	}

	/// Read the first frame of a verbatim track back as raw bytes.
	async fn read_frame(consumer: &moq_net::broadcast::Consumer, name: &str) -> Vec<u8> {
		let track = consumer.track(name).unwrap().subscribe(None).await.unwrap();
		let mut reader = Consumer::new(track, Container::Legacy(moq_mux::container::Kind::Data));
		let frame = tokio::time::timeout(Duration::from_secs(1), reader.read())
			.await
			.expect("verbatim read timed out")
			.unwrap()
			.expect("a published verbatim frame");
		frame.payload.to_vec()
	}
}
