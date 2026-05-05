use anyhow::Context;
use clap::ValueEnum;
use hang::moq_lite;
use tokio::io::AsyncWriteExt;

#[derive(ValueEnum, Clone, Copy)]
pub enum OutputFormat {
	Fmp4,
}

#[derive(clap::Args, Clone)]
pub struct SubscribeArgs {
	/// Output format for stdout
	#[arg(long)]
	pub output: OutputFormat,

	/// Maximum latency in milliseconds before skipping groups
	#[arg(long, default_value = "500")]
	pub max_latency: u64,
}

pub struct Subscribe {
	broadcast: moq_lite::BroadcastConsumer,
	args: SubscribeArgs,
}

impl Subscribe {
	pub fn new(broadcast: moq_lite::BroadcastConsumer, args: SubscribeArgs) -> Self {
		Self { broadcast, args }
	}

	pub async fn run(self) -> anyhow::Result<()> {
		match self.args.output {
			OutputFormat::Fmp4 => self.run_fmp4().await,
		}
	}

	async fn run_fmp4(self) -> anyhow::Result<()> {
		let mut stdout = tokio::io::stdout();
		let max_latency = std::time::Duration::from_millis(self.args.max_latency);

		// Read the first catalog snapshot up-front so we can write the init segment.
		// We re-subscribe a fresh CatalogConsumer for the muxer, so it sees catalog updates too.
		let catalog_track = self.broadcast.subscribe_track(&hang::Catalog::default_track())?;
		let mut catalog_consumer = hang::CatalogConsumer::new(catalog_track);
		let catalog = catalog_consumer.next().await?.context("empty catalog")?;

		// Build the merged init segment (ftyp + multi-track moov) from the catalog.
		let mut exporter = moq_mux::export::Fmp4::new(&catalog)?;
		let init = exporter.init(&catalog)?;
		stdout.write_all(&init).await?;
		stdout.flush().await?;

		// The muxer decodes both Legacy and CMAF tracks via Consumer<Hang> and yields
		// frames in timestamp order across tracks. We re-encode each frame as moof+mdat.
		let muxer_catalog_track = self.broadcast.subscribe_track(&hang::Catalog::default_track())?;
		let muxer_catalog = hang::CatalogConsumer::new(muxer_catalog_track);
		let mut muxer = moq_mux::export::Muxed::new(self.broadcast.clone(), muxer_catalog).with_latency(max_latency);

		while let Some(muxed) = muxer.read().await? {
			let fragment = exporter.frame(&muxed.track, &muxed.frame)?;
			stdout.write_all(&fragment).await?;
			stdout.flush().await?;
		}

		Ok(())
	}
}
