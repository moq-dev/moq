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
	/// Capture and publish a webcam (H.264, hardware-encoded when available).
	#[cfg(feature = "webcam")]
	Webcam(WebcamArgs),
}

/// Webcam capture options. See `moq-video` for the capture/encode details.
#[cfg(feature = "webcam")]
#[derive(clap::Args, Clone)]
pub struct WebcamArgs {
	/// Camera device. Platform-specific: an avfoundation index/name on macOS,
	/// a `/dev/videoN` path on Linux, or a dshow device name on Windows.
	/// Omit to use the default camera.
	#[arg(long)]
	pub device: Option<String>,

	/// Requested capture width. The camera snaps to its nearest supported mode.
	#[arg(long)]
	pub width: Option<u32>,

	/// Requested capture height.
	#[arg(long)]
	pub height: Option<u32>,

	/// Capture/encode framerate. Omit to use moq-video's default (30).
	#[arg(long)]
	pub fps: Option<u32>,

	/// Target bitrate in bits per second. Omit to derive one from the resolution.
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
	/// Capture a webcam. The producer is built on the blocking capture thread,
	/// so we just carry the catalog and config here.
	#[cfg(feature = "webcam")]
	Webcam {
		catalog: moq_mux::catalog::Producer,
		config: moq_video::encode::CameraConfig,
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
			#[cfg(feature = "webcam")]
			PublishFormat::Webcam(args) => Source::Webcam {
				catalog,
				config: args.clone().into(),
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
			#[cfg(feature = "webcam")]
			Source::Webcam { catalog, config } => {
				// Encodes on demand: the camera opens only while subscribed.
				// publish_camera drives the blocking capture loop internally.
				moq_video::encode::publish_camera(self.broadcast.clone(), catalog, config).await?;
				Ok(())
			}
		}
	}
}

#[cfg(feature = "webcam")]
impl From<WebcamArgs> for moq_video::encode::CameraConfig {
	fn from(args: WebcamArgs) -> Self {
		let kind = if args.software {
			moq_video::encode::EncoderKind::Software
		} else if args.hardware {
			moq_video::encode::EncoderKind::Hardware
		} else {
			moq_video::encode::EncoderKind::Auto
		};

		moq_video::encode::CameraConfig {
			camera: moq_video::camera::Config {
				device: args.device,
				width: args.width,
				height: args.height,
				framerate: args.fps,
			},
			bitrate: args.bitrate,
			kind,
		}
	}
}
