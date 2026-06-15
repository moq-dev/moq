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

pub struct Publish {
	source: PublishDecoder,
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
				PublishDecoder::Avc3(Box::new(avc3))
			}
			PublishFormat::Fmp4 => {
				let fmp4 = fmp4::Import::new(broadcast.clone(), catalog.clone());
				PublishDecoder::Fmp4(Box::new(fmp4))
			}
			PublishFormat::Ts => {
				let ts = ts::Import::new(broadcast.clone(), catalog.clone());
				PublishDecoder::Ts(Box::new(ts))
			}
			PublishFormat::Hls { playlist } => {
				let hls = hls::Import::new(broadcast.clone(), catalog.clone(), hls::Config::new(playlist.clone()))?;
				PublishDecoder::Hls(Box::new(hls))
			}
		};

		Ok(Self { source, broadcast })
	}

	pub fn consume(&self) -> moq_net::BroadcastConsumer {
		self.broadcast.consume()
	}

	pub async fn run(self) -> anyhow::Result<()> {
		match self.source {
			PublishDecoder::Hls(mut decoder) => {
				decoder.init().await?;
				decoder.run().await
			}
			mut decoder => {
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
		}
	}
}
