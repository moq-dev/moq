use clap::Subcommand;
use hang::moq_lite;
use moq_mux::producer;

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
	Avc3(Box<producer::Avc3>),
	Fmp4(Box<producer::Fmp4>),
	Hls(Box<producer::Hls>),
}

impl PublishDecoder {
	/// Decode a chunk of bytes from stdin (Avc3 or Fmp4 only).
	fn decode_buf(&mut self, buffer: &mut bytes::BytesMut) -> anyhow::Result<()> {
		match self {
			Self::Avc3(d) => d.decode_stream(buffer, None),
			Self::Fmp4(d) => d.decode(buffer),
			Self::Hls(_) => unreachable!(),
		}
	}
}

pub struct Publish {
	decoder: PublishDecoder,
	broadcast: moq_lite::BroadcastProducer,
}

impl Publish {
	pub fn new(format: &PublishFormat) -> anyhow::Result<Self> {
		let mut broadcast = moq_lite::Broadcast::new().produce();
		let catalog = moq_mux::CatalogProducer::new(&mut broadcast)?;

		let decoder = match format {
			PublishFormat::Avc3 => {
				let avc3 = producer::Avc3::new(broadcast.clone(), catalog.clone());
				PublishDecoder::Avc3(Box::new(avc3))
			}
			PublishFormat::Fmp4 => {
				let fmp4 = producer::Fmp4::new(broadcast.clone(), catalog.clone());
				PublishDecoder::Fmp4(Box::new(fmp4))
			}
			PublishFormat::Hls { playlist } => {
				let hls = producer::Hls::new(
					broadcast.clone(),
					catalog.clone(),
					producer::HlsConfig::new(playlist.clone()),
				)?;
				PublishDecoder::Hls(Box::new(hls))
			}
		};

		Ok(Self { decoder, broadcast })
	}

	pub fn consume(&self) -> moq_lite::BroadcastConsumer {
		self.broadcast.consume()
	}

	pub async fn run(mut self) -> anyhow::Result<()> {
		if let PublishDecoder::Hls(decoder) = &mut self.decoder {
			decoder.init().await?;
			decoder.run().await
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
