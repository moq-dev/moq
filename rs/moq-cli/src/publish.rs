use hang::moq_net;
use moq_mux::container::{flv, fmp4, ts};

/// Container format read from stdin on the import (source) side.
#[derive(Clone, Copy)]
pub enum PublishFormat {
	/// Raw AVC (H.264) Annex B elementary stream.
	Avc3,
	/// Fragmented MP4 (CMAF).
	Fmp4,
	/// MPEG-TS (transport stream).
	Ts,
	/// FLV (Flash Video / RTMP).
	Flv,
}

/// `clap` adapter for [`moq_video::encode::Codec`].
#[cfg(feature = "capture")]
#[derive(clap::ValueEnum, Clone, Copy, Default)]
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
#[derive(clap::Args, Clone)]
#[command(group = clap::ArgGroup::new("video-source").multiple(false))]
#[command(group = clap::ArgGroup::new("audio-source").multiple(false))]
pub struct CaptureArgs {
	/// Capture a camera, by the id `moq devices` reports (an AVFoundation
	/// `uniqueID` on macOS, a camera index / `/dev/videoN` path elsewhere).
	/// Bare `--camera`, or no source flag at all, opens the default camera.
	#[arg(long, num_args = 0..=1, group = "video-source")]
	pub camera: Option<Option<String>>,

	/// Capture a whole display, by the index `moq devices` reports. Bare
	/// `--display` captures the main display. On Linux the desktop portal opens a
	/// picker dialog and the index is ignored.
	#[arg(long, num_args = 0..=1, group = "video-source", alias = "screen")]
	pub display: Option<Option<String>>,

	/// Capture a single window, by the id `moq devices` reports. macOS only.
	#[arg(long, group = "video-source")]
	pub window: Option<String>,

	/// Capture every window of an application, by the bundle id `moq devices`
	/// reports. Windows opened later are included. macOS only.
	#[arg(long, group = "video-source")]
	pub app: Option<String>,

	/// Hide the mouse cursor. Display/window/app capture only.
	#[arg(long)]
	pub no_cursor: bool,

	/// Requested capture width. The source snaps to its nearest supported mode.
	#[arg(long)]
	pub width: Option<u32>,

	/// Requested capture height.
	#[arg(long)]
	pub height: Option<u32>,

	/// Capture/encode framerate. Omit to use the source's reported rate.
	#[arg(long)]
	pub fps: Option<u32>,

	/// Maximum video bitrate in bits per second. Omit to derive one from the resolution.
	///
	/// When publishing to a relay, the encoder backs off below this while the uplink is
	/// congested and climbs back afterwards; it never encodes above it.
	#[arg(long)]
	pub bitrate: Option<u64>,

	/// Video codec to encode. H.265 is hardware-only (VideoToolbox on macOS).
	#[arg(long, value_enum, default_value_t)]
	pub codec: VideoCodec,

	/// Force a hardware encoder (error if none is available).
	#[arg(long, conflicts_with = "software")]
	pub hardware: bool,

	/// Force the software encoder (openh264).
	#[arg(long)]
	pub software: bool,

	/// Capture a microphone, by the id `moq devices` reports. Bare
	/// `--microphone`, or no audio source flag, opens the default input.
	#[arg(long, num_args = 0..=1, group = "audio-source")]
	pub microphone: Option<Option<String>>,

	/// Capture the system (desktop) audio instead of a microphone: everything the
	/// machine is playing, minus this process. macOS only, and it needs the Screen
	/// Recording permission.
	#[arg(long, group = "audio-source")]
	pub system_audio: bool,

	/// Target audio bitrate in bits per second (Opus). Omit for the codec default.
	#[arg(long)]
	pub audio_bitrate: Option<u32>,

	/// Capture audio only (no camera).
	#[arg(long, conflicts_with = "no_audio", conflicts_with = "video-source")]
	pub no_video: bool,

	/// Capture video only (no microphone).
	#[arg(long, conflicts_with = "audio-source")]
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
			Self::Ts(d) => d.decode(chunk)?,
			Self::Flv(d) => d.decode(chunk)?,
		}
		Ok(())
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
			Self::Flv(d) => d.finish()?,
		}
		Ok(())
	}

	/// Abort the tracks with `err` instead of finishing, so subscribers see the
	/// real cause rather than `Error::Dropped`.
	fn abort(&mut self, err: moq_net::Error) {
		match self {
			Self::Avc3 { import, .. } => import.abort(err),
			Self::Fmp4(d) => d.abort(err),
			Self::Ts(d) => d.abort(err),
			Self::Flv(d) => d.abort(err),
		}
	}
}

// Exactly one Source exists per process, so the size gap between the small
// Stream variant and the larger Capture config is irrelevant.
#[allow(clippy::large_enum_variant)]
enum Source {
	/// Decode a container read from stdin.
	Stream(PublishDecoder),
	/// Capture from local devices. The per-medium producers are built on their
	/// own capture threads (native camera/screen capture, microphone via cpal), publishing
	/// onto the shared broadcast + catalog; [`Publish::run`] drives them
	/// concurrently.
	#[cfg(feature = "capture")]
	Capture {
		catalog: moq_mux::catalog::Producer,
		video: Option<(moq_video::capture::Config, moq_video::encode::Options)>,
		audio: Option<(moq_audio::capture::Config, moq_audio::encode::Options)>,
	},
}

/// A single-broadcast publisher: decodes stdin (or captures local devices) into
/// a broadcast that the MoQ side announces.
pub struct Publish {
	source: Source,
	// Keeps the origin-created broadcast alive for the publisher's lifetime;
	// tracks and importers don't. Only the capture path also writes through it.
	#[cfg_attr(not(feature = "capture"), allow(dead_code))]
	broadcast: moq_net::broadcast::Producer,
}

impl Publish {
	/// Build a publisher decoding the given container format from stdin into
	/// `broadcast` (typically created on the origin that announces it).
	pub fn new(mut broadcast: moq_net::broadcast::Producer, format: &PublishFormat) -> anyhow::Result<Self> {
		// TS carries undecoded elementary streams (SCTE-35, teletext, DVB AC-3, ...)
		// verbatim, so it uses the `mpegts` catalog extension rather than the media-only
		// `()`. The catalog producer owns the broadcast's catalog tracks, so each broadcast
		// gets exactly one; TS builds its `Ext` catalog here instead of the shared `()` below.
		if let PublishFormat::Ts = format {
			let catalog = moq_mux::catalog::Producer::with_catalog(
				&mut broadcast,
				moq_mux::catalog::hang::Catalog::<ts::Ext>::default(),
			)?;
			let ts = ts::Import::new(broadcast.clone(), catalog.reserve());
			return Ok(Self {
				source: Source::Stream(PublishDecoder::Ts(Box::new(ts))),
				broadcast,
			});
		}

		let catalog = moq_mux::catalog::Producer::new(&mut broadcast)?;
		let source = match format {
			PublishFormat::Avc3 => {
				let track = moq_mux::import::unique_track(&mut broadcast, ".avc3")?;
				let import = moq_mux::codec::h264::Import::new(track, catalog.reserve(), Default::default());
				let split = Box::new(moq_mux::codec::h264::Split::new());
				Source::Stream(PublishDecoder::Avc3 {
					split,
					import: Box::new(import),
				})
			}
			PublishFormat::Fmp4 => {
				let fmp4 = fmp4::Import::new(broadcast.clone(), catalog.reserve());
				Source::Stream(PublishDecoder::Fmp4(Box::new(fmp4)))
			}
			PublishFormat::Ts => unreachable!("TS is handled above with the mpegts catalog extension"),
			PublishFormat::Flv => {
				let flv = flv::Import::new(broadcast.clone(), catalog.reserve());
				Source::Stream(PublishDecoder::Flv(Box::new(flv)))
			}
		};

		Ok(Self { source, broadcast })
	}

	/// Build a publisher capturing local devices (camera/screen and microphone).
	///
	/// `bandwidth` is the uplink's send estimate, when there is one: the video
	/// encoder follows it down while the link is congested rather than
	/// overshooting a pipe that can't carry it. Pass `None` to encode at the
	/// configured bitrate regardless.
	#[cfg(feature = "capture")]
	pub fn capture(
		mut broadcast: moq_net::broadcast::Producer,
		args: &CaptureArgs,
		bandwidth: Option<moq_net::bandwidth::Consumer>,
	) -> anyhow::Result<Self> {
		let catalog = moq_mux::catalog::Producer::new(&mut broadcast)?;

		let video = (!args.no_video).then(|| (args.video_config(), args.video_encode(bandwidth)));
		let audio = (!args.no_audio).then(|| (args.audio_config(), args.audio_encode()));
		anyhow::ensure!(video.is_some() || audio.is_some(), "nothing to capture");

		Ok(Self {
			source: Source::Capture { catalog, video, audio },
			broadcast,
		})
	}

	/// Drive the source until stdin EOF (or the capture devices stop).
	pub async fn run(self) -> anyhow::Result<()> {
		match self.source {
			Source::Stream(mut decoder) => {
				let mut stdin = tokio::io::stdin();
				let mut buffer = bytes::BytesMut::new();

				// Run the read/decode loop so an error surfaces here rather than
				// dropping the decoder (and its tracks) with a bare Error::Dropped.
				let result: anyhow::Result<()> = async {
					loop {
						buffer.clear();
						let n = tokio::io::AsyncReadExt::read_buf(&mut stdin, &mut buffer).await?;
						if n == 0 {
							return Ok(()); // EOF
						}
						decoder.decode_chunk(&buffer)?;
					}
				}
				.await;

				// Flush on a clean EOF; on any error (read, decode, or the flush
				// itself) abort with the real cause so subscribers see it instead of
				// a bare Error::Dropped.
				let outcome = result.and_then(|()| decoder.finish());
				if let Err(err) = &outcome {
					decoder.abort(moq_net::Error::Transport(err.to_string()));
				}
				outcome
			}
			#[cfg(feature = "capture")]
			Source::Capture { catalog, video, audio } => {
				// Each enabled medium publishes its own track onto the shared
				// broadcast + catalog. A single shared clock keeps the audio and
				// video timelines aligned even though the devices open at
				// different times. Video encodes on demand (camera opens only
				// while subscribed); audio (cpal) is blocking, so it runs on a
				// dedicated thread.
				let clock = moq_mux::Clock::new();
				let video_fut = {
					let broadcast = self.broadcast.clone();
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
				let audio_fut = {
					let broadcast = self.broadcast.clone();
					async move {
						match audio {
							Some((config, encode)) => {
								moq_audio::encode::publish_capture(broadcast, catalog, config, encode, clock)
									.await
									.map_err(anyhow::Error::from)
							}
							None => Ok(()),
						}
					}
				};

				tokio::try_join!(video_fut, audio_fut)?;
				Ok(())
			}
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

	fn video_config(&self) -> moq_video::capture::Config {
		let mut config = moq_video::capture::Config::default();
		config.source = self.video_source();
		config.width = self.width;
		config.height = self.height;
		config.framerate = self.fps;
		config.cursor = !self.no_cursor;
		config
	}

	fn video_encode(&self, bandwidth: Option<moq_net::bandwidth::Consumer>) -> moq_video::encode::Options {
		let mut options = moq_video::encode::Options::default();
		options.bitrate = self.bitrate;
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
	fn audio_encode(&self) -> moq_audio::encode::Options {
		let mut options = moq_audio::encode::Options::default();
		options.bitrate = self.audio_bitrate;
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
	/// Let the origin's spawned attach task run: a created broadcast becomes
	/// routable asynchronously, shortly after `create_broadcast` returns.
	async fn settle() {
		for _ in 0..10 {
			tokio::task::yield_now().await;
		}
	}

	async fn manufacture_input() -> Vec<u8> {
		// Create the broadcast on a throwaway origin so the exporter can resolve it by path.
		let origin = moq_net::Origin::random().produce();
		let mut broadcast = origin
			.create_broadcast("cli", moq_net::broadcast::Route::new().with_announce(true))
			.unwrap();
		settle().await;
		let mut catalog =
			moq_mux::catalog::Producer::with_catalog(&mut broadcast, Catalog::<tscat::Ext>::default()).unwrap();

		// Section-framed verbatim stream (SCTE-35, stream_type 0x86).
		let section = broadcast
			.unique_track(".scte35", hang::container::track_info())
			.unwrap();
		let mut section_track = tscat::Track::new(SECTION_PID);
		section_track.verbatim = Some(tscat::Verbatim::new(0x86, tscat::Framing::Section));
		catalog
			.lock()
			.mpegts
			.tracks
			.insert(section.name().to_string(), section_track);
		let mut section_producer = Producer::new(section, Container::Legacy);
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
		let pes = broadcast.unique_track(".data", hang::container::track_info()).unwrap();
		let mut verbatim = tscat::Verbatim::new(0x06, tscat::Framing::Pes);
		verbatim.stream_id = Some(VERBATIM_PES_STREAM_ID);
		let mut pes_track = tscat::Track::new(VERBATIM_PES_PID);
		pes_track.verbatim = Some(verbatim);
		catalog.lock().mpegts.tracks.insert(pes.name().to_string(), pes_track);
		let mut pes_producer = Producer::new(pes, Container::Legacy);
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
				.with_latency(Duration::ZERO),
		)
		.await
	}

	/// Full CLI round-trip: a TS feed with undecoded streams goes through `Publish`
	/// (which selects the `mpegts` catalog) and the subscribe-side `Export::with_ts`,
	/// and the SCTE-35 section and the verbatim PES survive with their PIDs, framing,
	/// PES stream_id, and byte-exact payloads.
	#[tokio::test(start_paused = true)]
	async fn ts_verbatim_streams_round_trip_through_cli() {
		// Paused time auto-advances when the exporter parks, so the `drain` timeouts
		// fire instantly instead of waiting on the wall clock.
		let input = manufacture_input().await;

		// Publish side: `Publish::new(Ts)` builds a `ts::Import<Ext>`, so the verbatim
		// streams land in the broadcast instead of being dropped by the media-only path.
		// The broadcast is created on a throwaway origin so the exporter can resolve it by path.
		let origin = moq_net::Origin::random().produce();
		let broadcast = origin
			.create_broadcast("cli", moq_net::broadcast::Route::new().with_announce(true))
			.unwrap();
		settle().await;
		let mut publish = Publish::new(broadcast, &PublishFormat::Ts).unwrap();
		#[allow(irrefutable_let_patterns)]
		let Source::Stream(decoder) = &mut publish.source else {
			panic!("expected a stream source");
		};
		decoder.decode_chunk(&input).unwrap();
		decoder.finish().unwrap();

		// Subscribe side: the same `with_ts` call `run_ts` makes, re-emitting the
		// ancillary streams verbatim.
		let output = drain(
			Export::with_ts(moq_mux::Source::new(origin.consume(), "cli"), CatalogFormat::Hang)
				.await
				.unwrap()
				.with_latency(Duration::ZERO),
		)
		.await;

		// Re-import the round-tripped TS and inspect the recovered `mpegts` section.
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let consumer = broadcast.consume();
		let catalog =
			moq_mux::catalog::Producer::with_catalog(&mut broadcast, Catalog::<tscat::Ext>::default()).unwrap();
		let mut import = Import::new(broadcast, catalog.reserve());
		import.decode(&BytesMut::from(&output[..])).unwrap();
		import.finish().unwrap();
		let snapshot = catalog.snapshot();

		let (section_name, section) = snapshot
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

	/// Read the first frame of a verbatim track back as raw bytes.
	async fn read_frame(consumer: &moq_net::broadcast::Consumer, name: &str) -> Vec<u8> {
		let track = consumer.track(name).unwrap().subscribe(None).await.unwrap();
		let mut reader = Consumer::new(track, Container::Legacy).with_latency(Duration::ZERO);
		let frame = tokio::time::timeout(Duration::from_secs(1), reader.read())
			.await
			.expect("verbatim read timed out")
			.unwrap()
			.expect("a published verbatim frame");
		frame.payload.to_vec()
	}
}
