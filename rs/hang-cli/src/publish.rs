use bytes::BytesMut;
use clap::Subcommand;
use hang::{
	import::{Decoder, DecoderFormat, Fmp4, ImportMode},
	moq_lite::BroadcastConsumer,
	BroadcastProducer,
};
use tokio::io::AsyncReadExt;

#[derive(Subcommand, Clone)]
pub enum PublishFormat {
	Avc3,
	/// fMP4 format for WebCodecs (frame-by-frame)
	Fmp4,
	/// fMP4 format for MSE (complete segments)
	Fmp4Mse,
	// NOTE: No aac support because it needs framing.
	Hls {
		/// URL or file path of an HLS playlist to ingest.
		#[arg(long)]
		playlist: String,
	},
}

enum PublishDecoder {
	Decoder(Box<hang::import::Decoder>),
	Fmp4(Box<hang::import::Fmp4>),
	Hls(Box<hang::import::Hls>),
}

pub struct Publish {
	decoder: PublishDecoder,
	broadcast: BroadcastProducer,
	buffer: BytesMut,
}

impl Publish {
	pub fn new(format: &PublishFormat) -> anyhow::Result<Self> {
		let broadcast = BroadcastProducer::default();

		let decoder = match format {
			PublishFormat::Avc3 => {
				let format = DecoderFormat::Avc3;
				let stream = Decoder::new(broadcast.clone(), format);
				PublishDecoder::Decoder(Box::new(stream))
			}
			PublishFormat::Fmp4 => {
				let format = DecoderFormat::Fmp4;
				let stream = Decoder::new(broadcast.clone(), format);
				PublishDecoder::Decoder(Box::new(stream))
			}
			PublishFormat::Fmp4Mse => {
				// Use Fmp4 directly with Segments mode for MSE
				let fmp4 = Fmp4::with_mode(broadcast.clone(), ImportMode::Segments);
				PublishDecoder::Fmp4(Box::new(fmp4))
			}
			PublishFormat::Hls { playlist } => {
				let hls = hang::import::Hls::new(
					broadcast.clone(),
					hang::import::HlsConfig {
						playlist: playlist.clone(),
						client: None,
					},
				)?;
				PublishDecoder::Hls(Box::new(hls))
			}
		};

		Ok(Self {
			decoder,
			buffer: BytesMut::new(),
			broadcast,
		})
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
			PublishDecoder::Fmp4(decoder) => {
				let mut input = tokio::io::stdin();

				while !decoder.is_initialized() && input.read_buf(&mut self.buffer).await? > 0 {
					decoder.decode(&mut self.buffer)?;
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
			PublishDecoder::Fmp4(decoder) => {
				let mut input = tokio::io::stdin();

				while input.read_buf(&mut self.buffer).await? > 0 {
					decoder.decode(&mut self.buffer)?;
				}
			}
			PublishDecoder::Hls(decoder) => decoder.run().await?,
		}

		Ok(())
	}
}
