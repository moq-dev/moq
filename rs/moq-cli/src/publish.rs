use clap::Subcommand;
use hang::moq_net;
use moq_mux::container::{fmp4, hls};

#[derive(Subcommand, Clone)]
pub enum PublishFormat {
	Avc3,
	Fmp4,
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
	Hls(Box<hls::Import>),
}

impl PublishDecoder {
	/// Decode a chunk of bytes from stdin (Avc3 or Fmp4 only).
	fn decode_buf(&mut self, buffer: &mut bytes::BytesMut) -> anyhow::Result<()> {
		match self {
			Self::Avc3(d) => Ok(d.decode_stream(buffer, None)?),
			Self::Fmp4(d) => Ok(d.decode(buffer)?),
			Self::Hls(_) => unreachable!(),
		}
	}
}

pub struct Publish {
	decoder: PublishDecoder,
	broadcast: moq_net::BroadcastProducer,
}

impl Publish {
	pub fn new(format: &PublishFormat) -> anyhow::Result<Self> {
		let mut broadcast = moq_net::Broadcast::new().produce();
		let catalog = moq_mux::catalog::hang::Producer::new(&mut broadcast)?;

		let decoder = match format {
			PublishFormat::Avc3 => {
				let avc3 = moq_mux::codec::h264::Import::new(broadcast.clone(), catalog.clone())
					.with_mode(moq_mux::codec::h264::Mode::Avc3)?;
				PublishDecoder::Avc3(Box::new(avc3))
			}
			PublishFormat::Fmp4 => {
				let fmp4 = fmp4::Import::new(broadcast.clone(), catalog.clone());
				PublishDecoder::Fmp4(Box::new(fmp4))
			}
			PublishFormat::Hls { playlist } => {
				let hls = hls::Import::new(broadcast.clone(), catalog.clone(), hls::Config::new(playlist.clone()))?;
				PublishDecoder::Hls(Box::new(hls))
			}
		};

		Ok(Self { decoder, broadcast })
	}

	pub fn consume(&self) -> moq_net::BroadcastConsumer {
		self.broadcast.consume()
	}

	pub async fn run(mut self) -> anyhow::Result<()> {
		if let PublishDecoder::Hls(decoder) = &mut self.decoder {
			decoder.init().await?;
			Ok(decoder.run().await?)
		} else {
			let mut stdin = tokio::io::stdin();
			let mut buffer = bytes::BytesMut::new();

			loop {
				let n = tokio::io::AsyncReadExt::read_buf(&mut stdin, &mut buffer).await?;
				if n == 0 {
					return Ok(());
				}
				self.decoder.decode_buf(&mut buffer)?;
			}
		}
	}
}
