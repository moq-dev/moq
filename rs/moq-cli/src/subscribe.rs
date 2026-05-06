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

		// Fmp4 subscribes to the catalog internally, builds the merged init segment
		// from the first catalog snapshot, then yields moof+mdat fragments in
		// timestamp order across tracks.
		let mut fmp4 = moq_mux::export::Fmp4::new(self.broadcast)?.with_latency(max_latency);

		while let Some(chunk) = fmp4.next().await? {
			stdout.write_all(&chunk).await?;
			stdout.flush().await?;
		}

		Ok(())
	}
}
