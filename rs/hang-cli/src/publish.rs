use bytes::BytesMut;
use clap::Subcommand;
use hang::{
	import::{Decoder, DecoderFormat},
	moq_lite::BroadcastConsumer,
	BroadcastProducer,
};
use tokio::io::AsyncReadExt;
use url::Url;

#[derive(Subcommand, Clone)]
pub enum PublishFormat {
	Avc3,
	Fmp4,
	// NOTE: No aac support because it needs framing.
	Hls {
		/// URL of an HLS playlist to ingest.
		#[arg(long)]
		playlist: Url,
	},
}

enum PublishDecoder {
	Decoder(hang::import::Decoder),
	Hls(hang::import::Hls),
}

pub struct Publish {
	decoder: PublishDecoder,
	broadcast: BroadcastProducer,
	buffer: BytesMut,
}

impl Publish {
	pub fn new(format: &PublishFormat) -> Self {
		let broadcast = BroadcastProducer::default();

		let decoder = match format {
			PublishFormat::Avc3 => {
				let format = DecoderFormat::Avc3;
				let stream = Decoder::new(broadcast.clone(), format);
				PublishDecoder::Decoder(stream)
			}
			PublishFormat::Fmp4 => {
				let format = DecoderFormat::Fmp4;
				let stream = Decoder::new(broadcast.clone(), format);
				PublishDecoder::Decoder(stream)
			}
			PublishFormat::Hls { playlist } => {
				let hls = hang::import::Hls::new(
					broadcast.clone(),
					hang::import::HlsConfig {
						playlist: playlist.clone(),
						client: None,
					},
				);
				PublishDecoder::Hls(hls)
			}
		};

		Self {
			decoder,
			buffer: BytesMut::new(),
			broadcast,
		}
	}

	pub fn consume(&self) -> BroadcastConsumer {
		self.broadcast.consume()
	}
}

impl Publish {
	pub async fn init(&mut self) -> anyhow::Result<()> {
		match &mut self.decoder {
			PublishDecoder::Decoder(decoder) => {
				let mut input = tokio::io::stdin();

				while !decoder.is_initialized() && input.read_buf(&mut self.buffer).await? > 0 {
					decoder.decode_stream(&mut self.buffer)?;
				}
			}
			PublishDecoder::Hls(decoder) => decoder.init().await?,
		}

		Ok(())
	}

	pub async fn run(mut self) -> anyhow::Result<()> {
		match &mut self.decoder {
			PublishDecoder::Decoder(decoder) => {
				let mut input = tokio::io::stdin();

				while input.read_buf(&mut self.buffer).await? > 0 {
					decoder.decode_stream(&mut self.buffer)?;
				}
			}
			PublishDecoder::Hls(decoder) => decoder.run().await?,
		}

		Ok(())
	}
}
