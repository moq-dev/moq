use std::time::Duration;

use clap::ValueEnum;
use hang::moq_net;
use tokio::io::AsyncWriteExt;

#[derive(ValueEnum, Clone, Copy)]
pub enum SubscribeFormat {
	Fmp4,
	Mkv,
}

/// Catalog wire format to subscribe to for track discovery.
#[derive(ValueEnum, Clone, Copy, Default)]
pub enum CatalogFormat {
	/// The hang catalog (`catalog.json`, hang JSON schema).
	#[default]
	Hang,
	/// The MSF catalog (`catalog`, draft-ietf-moq-msf JSON schema).
	Msf,
}

impl From<CatalogFormat> for moq_mux::export::CatalogFormat {
	fn from(format: CatalogFormat) -> Self {
		match format {
			CatalogFormat::Hang => Self::Hang,
			CatalogFormat::Msf => Self::Msf,
		}
	}
}

#[derive(clap::Args, Clone)]
pub struct SubscribeArgs {
	/// The format to write to stdout.
	#[arg(long)]
	pub format: SubscribeFormat,

	/// Maximum latency before skipping groups (e.g. `500ms`, `1s`).
	#[arg(long, default_value = "500ms", value_parser = humantime::parse_duration)]
	pub max_latency: Duration,

	/// Cap the output fragment duration (e.g. `2s`, `500ms`).
	///
	/// By default a fragment covers one GOP (rolled over on video keyframes).
	/// Setting this caps each fragment to roughly the given duration.
	/// The cap applies in addition to GOP rollover.
	#[arg(long, value_parser = humantime::parse_duration)]
	pub fragment_duration: Option<Duration>,

	/// Catalog format to subscribe to for track discovery.
	#[arg(long, default_value = "hang")]
	pub catalog: CatalogFormat,
}

pub struct Subscribe {
	broadcast: moq_net::BroadcastConsumer,
	args: SubscribeArgs,
}

impl Subscribe {
	pub fn new(broadcast: moq_net::BroadcastConsumer, args: SubscribeArgs) -> Self {
		Self { broadcast, args }
	}

	pub async fn run(self) -> anyhow::Result<()> {
		match self.args.format {
			SubscribeFormat::Fmp4 => self.run_fmp4().await,
			SubscribeFormat::Mkv => self.run_mkv().await,
		}
	}

	async fn run_fmp4(self) -> anyhow::Result<()> {
		let mut stdout = tokio::io::stdout();

		// Fmp4 subscribes to the catalog internally, builds the merged init segment
		// from the first catalog snapshot, then yields moof+mdat fragments in
		// timestamp order across tracks.
		let mut fmp4 = moq_mux::export::Fmp4::new(self.broadcast, self.args.catalog.into())?
			.with_latency(self.args.max_latency)
			.with_fragment_duration(self.args.fragment_duration);

		while let Some(chunk) = fmp4.next().await? {
			stdout.write_all(&chunk).await?;
			stdout.flush().await?;
		}

		Ok(())
	}

	async fn run_mkv(self) -> anyhow::Result<()> {
		let mut stdout = tokio::io::stdout();

		// Mkv writes EBML + an unknown-size Segment header, then per-fragment
		// Cluster elements. Avc3/Hev1 sources are transcoded to avc1/hvc1
		// shape internally (synthesizing avcC/hvcC from inline parameter sets).
		let mut mkv = moq_mux::export::Mkv::new(self.broadcast, self.args.catalog.into())?
			.with_latency(self.args.max_latency)
			.with_fragment_duration(self.args.fragment_duration);

		while let Some(chunk) = mkv.next().await? {
			stdout.write_all(&chunk).await?;
			stdout.flush().await?;
		}

		Ok(())
	}
}
