use clap::ValueEnum;
use hang::moq_lite;
use moq_mux::import;

#[derive(ValueEnum, Clone, Copy)]
pub enum InputFormat {
	Fmp4,
	Avc3,
}

#[derive(ValueEnum, Clone, Copy)]
pub enum ExportFormat {
	Hang,
	Fmp4,
}

#[derive(clap::Args, Clone)]
pub struct PublishArgs {
	/// Input format (what's being read from stdin)
	#[arg(long)]
	pub input: InputFormat,

	/// Optional: convert to a different format before publishing.
	/// If not specified, publishes in the import's native format.
	#[arg(long)]
	pub export: Option<ExportFormat>,
}

enum PublishDecoder {
	Avc3(Box<import::Avc3>),
	Fmp4(Box<import::Fmp4>),
}

pub struct Publish {
	decoder: PublishDecoder,
	broadcast: moq_lite::BroadcastProducer,
}

impl Publish {
	pub fn new(args: &PublishArgs) -> anyhow::Result<Self> {
		let mut broadcast = moq_lite::Broadcast::new().produce();
		let catalog = moq_mux::CatalogProducer::new(&mut broadcast)?;

		let decoder = match args.input {
			InputFormat::Avc3 => {
				let avc3 = import::Avc3::new(broadcast.clone(), catalog.clone());
				PublishDecoder::Avc3(Box::new(avc3))
			}
			InputFormat::Fmp4 => {
				let fmp4 = import::Fmp4::new(broadcast.clone(), catalog.clone());
				PublishDecoder::Fmp4(Box::new(fmp4))
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
		}
	}
}
