use clap::Subcommand;
use hang::moq_net;
use moq_mux::container::{fmp4, hls, ts};

#[derive(Subcommand, Clone)]
pub enum PublishFormat {
	Avc3,
	Fmp4,
	/// MPEG-TS (transport stream) read from stdin.
	Ts,
	// NOTE: No aac support because it needs framing.
	Hls {
		/// URL or file path of an HLS playlist to ingest.
		#[arg(long)]
		playlist: String,
	},
	/// Capture and publish the camera (H.264) and microphone (Opus).
	#[cfg(feature = "capture")]
	Capture(CaptureArgs),
}

/// Device capture options. Video (camera -> H.264) maps to `moq-video`; audio
/// (microphone -> Opus) to `moq-audio`. Both are captured by default; use
/// `--no-video` / `--no-audio` to publish only one.
#[cfg(feature = "capture")]
#[derive(clap::Args, Clone)]
pub struct CaptureArgs {
	/// Camera device. Platform-specific: an avfoundation index/name on macOS,
	/// a `/dev/videoN` path on Linux, or a dshow device name on Windows.
	/// Omit to use the default camera.
	#[arg(long)]
	pub camera: Option<String>,

	/// Requested capture width. The camera snaps to its nearest supported mode.
	#[arg(long)]
	pub width: Option<u32>,

	/// Requested capture height.
	#[arg(long)]
	pub height: Option<u32>,

	/// Capture/encode framerate. Omit to use the camera's reported rate.
	#[arg(long)]
	pub fps: Option<u32>,

	/// Target video bitrate in bits per second. Omit to derive one from the resolution.
	#[arg(long)]
	pub bitrate: Option<u64>,

	/// Force a hardware encoder (error if none is available).
	#[arg(long, conflicts_with = "software")]
	pub hardware: bool,

	/// Force the software encoder (libx264).
	#[arg(long)]
	pub software: bool,

	/// Microphone device name. Omit to use the default input.
	#[arg(long)]
	pub microphone: Option<String>,

	/// Target audio bitrate in bits per second (Opus). Omit for the codec default.
	#[arg(long)]
	pub audio_bitrate: Option<u32>,

	/// Capture audio only (no camera).
	#[arg(long, conflicts_with = "no_audio")]
	pub no_video: bool,

	/// Capture video only (no microphone).
	#[arg(long)]
	pub no_audio: bool,
}

enum PublishDecoder {
	Avc3(Box<moq_mux::codec::h264::Import>),
	Fmp4(Box<fmp4::Import>),
	Ts(Box<ts::Import>),
	Hls(Box<hls::Import>),
}

impl PublishDecoder {
	/// Decode a chunk of bytes from stdin (Avc3, Fmp4, or Ts).
	fn decode_buf(&mut self, buffer: &mut bytes::BytesMut) -> anyhow::Result<()> {
		match self {
			Self::Avc3(d) => d.decode_stream(buffer, None),
			Self::Fmp4(d) => d.decode(buffer),
			Self::Ts(d) => d.decode(buffer),
			Self::Hls(_) => unreachable!(),
		}
	}
}

// Exactly one Source exists per process, so the size gap between the small
// Stream variant and the larger Capture config is irrelevant.
#[allow(clippy::large_enum_variant)]
enum Source {
	/// Decode a container read from stdin (or an HLS playlist).
	Stream(PublishDecoder),
	/// Capture from local devices. The per-medium producers are built on their
	/// own capture threads (camera via ffmpeg, microphone via cpal), publishing
	/// onto the shared broadcast + catalog; [`Publish::run`] drives them
	/// concurrently.
	#[cfg(feature = "capture")]
	Capture {
		catalog: moq_mux::catalog::Producer,
		video: Option<(moq_video::capture::Config, moq_video::encode::Options)>,
		audio: Option<(moq_audio::capture::Config, moq_audio::EncoderOutput)>,
	},
}

pub struct Publish {
	source: Source,
	broadcast: moq_net::BroadcastProducer,
}

impl Publish {
	pub fn new(format: &PublishFormat) -> anyhow::Result<Self> {
		let mut broadcast = moq_net::Broadcast::new().produce();
		let catalog = moq_mux::catalog::Producer::new(&mut broadcast)?;

		let source = match format {
			PublishFormat::Avc3 => {
				let avc3 = moq_mux::codec::h264::Import::new(broadcast.clone(), catalog.clone())
					.with_mode(moq_mux::codec::h264::Mode::Avc3)?;
				Source::Stream(PublishDecoder::Avc3(Box::new(avc3)))
			}
			PublishFormat::Fmp4 => {
				let fmp4 = fmp4::Import::new(broadcast.clone(), catalog.clone());
				Source::Stream(PublishDecoder::Fmp4(Box::new(fmp4)))
			}
			PublishFormat::Ts => {
				let ts = ts::Import::new(broadcast.clone(), catalog.clone());
				Source::Stream(PublishDecoder::Ts(Box::new(ts)))
			}
			PublishFormat::Hls { playlist } => {
				let hls = hls::Import::new(broadcast.clone(), catalog.clone(), hls::Config::new(playlist.clone()))?;
				Source::Stream(PublishDecoder::Hls(Box::new(hls)))
			}
			#[cfg(feature = "capture")]
			PublishFormat::Capture(args) => {
				let video = (!args.no_video).then(|| (args.video_config(), args.video_encode()));
				let audio = (!args.no_audio).then(|| (args.audio_config(), args.audio_encode()));
				anyhow::ensure!(video.is_some() || audio.is_some(), "nothing to capture");
				Source::Capture { catalog, video, audio }
			}
		};

		Ok(Self { source, broadcast })
	}

	pub fn consume(&self) -> moq_net::BroadcastConsumer {
		self.broadcast.consume()
	}

	pub async fn run(self) -> anyhow::Result<()> {
		match self.source {
			Source::Stream(PublishDecoder::Hls(mut decoder)) => {
				decoder.init().await?;
				decoder.run().await
			}
			Source::Stream(mut decoder) => {
				let mut stdin = tokio::io::stdin();
				let mut buffer = bytes::BytesMut::new();

				loop {
					let n = tokio::io::AsyncReadExt::read_buf(&mut stdin, &mut buffer).await?;
					if n == 0 {
						return Ok(());
					}
					decoder.decode_buf(&mut buffer)?;
				}
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
							Some((config, output)) => moq_audio::capture::publish_microphone(
								broadcast, catalog, config, "audio", output, clock,
							)
							.await
							.map_err(anyhow::Error::from),
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
	fn video_config(&self) -> moq_video::capture::Config {
		let mut config = moq_video::capture::Config::default();
		config.device = self.camera.clone();
		config.width = self.width;
		config.height = self.height;
		config.framerate = self.fps;
		config
	}

	fn video_encode(&self) -> moq_video::encode::Options {
		let mut options = moq_video::encode::Options::default();
		options.bitrate = self.bitrate;
		options.kind = if self.software {
			moq_video::encode::Kind::Software
		} else if self.hardware {
			moq_video::encode::Kind::Hardware
		} else {
			moq_video::encode::Kind::Auto
		};
		options
	}

	fn audio_config(&self) -> moq_audio::capture::Config {
		let mut config = moq_audio::capture::Config::default();
		config.device = self.microphone.clone();
		config
	}

	fn audio_encode(&self) -> moq_audio::EncoderOutput {
		moq_audio::EncoderOutput {
			bitrate: self.audio_bitrate,
			..Default::default()
		}
	}
}
