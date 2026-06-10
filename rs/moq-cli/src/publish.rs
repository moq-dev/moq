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
	/// Capture and publish from local devices (camera now; microphone planned).
	#[cfg(feature = "capture")]
	Capture(CaptureArgs),
}

/// Device capture options. Video flags map to `moq-video`; audio capture
/// (microphone -> Opus via `moq-audio`) will add `--microphone` etc. here.
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

enum Source {
	/// Decode a container read from stdin (or an HLS playlist).
	Stream(PublishDecoder),
	/// Capture from local devices. The per-medium producers are built on their
	/// own capture threads, so we just carry the shared catalog and the configs.
	///
	/// Audio (microphone -> Opus via `moq-audio`) plugs in as sibling fields
	/// here plus a second concurrent producer in [`Publish::run`]; the
	/// broadcast and catalog are already shared, so it's purely additive.
	#[cfg(feature = "capture")]
	Capture {
		catalog: moq_mux::catalog::Producer,
		video: moq_video::capture::Config,
		video_encode: moq_video::encode::Options,
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
			PublishFormat::Capture(args) => Source::Capture {
				catalog,
				video: args.capture_config(),
				video_encode: args.encode_options(),
			},
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
			Source::Capture {
				catalog,
				video,
				video_encode,
			} => {
				// Encodes on demand: the camera opens only while subscribed.
				// publish_capture drives the blocking capture loop internally.
				// When audio lands, run the mic producer concurrently here
				// (e.g. tokio::try_join!) on the same broadcast + catalog.
				moq_video::encode::publish_capture(self.broadcast.clone(), catalog, video, video_encode).await?;
				Ok(())
			}
		}
	}
}

#[cfg(feature = "capture")]
impl CaptureArgs {
	fn capture_config(&self) -> moq_video::capture::Config {
		let mut config = moq_video::capture::Config::default();
		config.device = self.camera.clone();
		config.width = self.width;
		config.height = self.height;
		config.framerate = self.fps;
		config
	}

	fn encode_options(&self) -> moq_video::encode::Options {
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
}
