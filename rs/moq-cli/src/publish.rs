use clap::Subcommand;
use hang::moq_lite;
use moq_mux::import;

#[derive(Subcommand, Clone)]
pub enum PublishFormat {
	Avc3,
	Fmp4 {
		/// Transmit the fMP4 container directly instead of decoding it.
		#[arg(long)]
		passthrough: bool,
	},
	// NOTE: No aac support because it needs framing.
	Hls {
		/// URL or file path of an HLS playlist to ingest.
		#[arg(long)]
		playlist: String,

		/// Transmit the fMP4 segments directly instead of decoding them.
		#[arg(long)]
		passthrough: bool,
	},
}

enum PublishDecoder {
	Avc3(Box<import::Avc3>),
	Fmp4(Box<import::Fmp4>),
	Hls(Box<import::Hls>),
}

pub struct Publish {
	decoder: PublishDecoder,
	broadcast: moq_lite::BroadcastProducer,
}

impl Publish {
	pub fn new(format: &PublishFormat) -> anyhow::Result<Self> {
		let mut broadcast = moq_lite::BroadcastProducer::default();

		let catalog = hang::Catalog::default().produce();
		broadcast.insert_track(catalog.track.clone());

		let decoder = match format {
			PublishFormat::Avc3 => {
				let avc3 = import::Avc3::new(broadcast.clone(), catalog.clone());
				PublishDecoder::Avc3(Box::new(avc3))
			}
			PublishFormat::Fmp4 { passthrough } => {
				let fmp4 = import::Fmp4::new(
					broadcast.clone(),
					catalog.clone(),
					import::Fmp4Config {
						passthrough: *passthrough,
					},
				);
				PublishDecoder::Fmp4(Box::new(fmp4))
			}
			PublishFormat::Hls { playlist, passthrough } => {
				let hls = import::Hls::new(
					broadcast.clone(),
					catalog.clone(),
					import::HlsConfig {
						playlist: playlist.clone(),
						client: None,
						passthrough: *passthrough,
					},
				)?;
				PublishDecoder::Hls(Box::new(hls))
			}
		};

		Ok(Self { decoder, broadcast })
	}

	pub fn consume(&self) -> moq_lite::BroadcastConsumer {
		self.broadcast.consume()
	}
}

impl Publish {
	pub async fn run(mut self) -> anyhow::Result<()> {
		let mut stdin = tokio::io::stdin();

		match &mut self.decoder {
			PublishDecoder::Avc3(decoder) => decoder.decode_from(&mut stdin).await,
			PublishDecoder::Fmp4(decoder) => decoder.decode_from(&mut stdin).await,
			PublishDecoder::Hls(decoder) => decoder.run().await,
		}
	}
}
