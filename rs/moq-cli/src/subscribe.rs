use std::time::Duration;

use clap::ValueEnum;
use hang::moq_net;
use moq_mux::catalog::CatalogFormat;
use tokio::io::AsyncWriteExt;

#[derive(ValueEnum, Clone, Copy)]
pub enum SubscribeFormat {
	Fmp4,
	Mkv,
	Ts,
	Flv,
}

/// `clap` adapter for [`CatalogFormat`] (which is `#[non_exhaustive]` and so
/// can't derive `ValueEnum` itself).
#[derive(ValueEnum, Clone, Copy)]
pub enum CatalogFormatArg {
	Hang,
	#[value(name = "hangz")]
	HangZ,
	Msf,
}

impl From<CatalogFormatArg> for CatalogFormat {
	fn from(format: CatalogFormatArg) -> Self {
		match format {
			CatalogFormatArg::Hang => Self::Hang,
			CatalogFormatArg::HangZ => Self::HangZ,
			CatalogFormatArg::Msf => Self::Msf,
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
	///
	/// When omitted, the format is auto-detected from the broadcast name suffix
	/// (`.hang` -> hang, `.msf` -> msf), falling back to hang. Pass `hangz` to read
	/// the DEFLATE-compressed `catalog.json.z` track instead (same `.hang` broadcast).
	#[arg(long)]
	pub catalog: Option<CatalogFormatArg>,
}

impl SubscribeArgs {
	/// Resolve the catalog format, falling back to detection from the broadcast
	/// name suffix and then to the default.
	pub fn catalog_format(&self, broadcast: &str) -> CatalogFormat {
		self.catalog
			.map(Into::into)
			.or_else(|| CatalogFormat::detect(broadcast))
			.unwrap_or_default()
	}
}

pub struct Subscribe {
	broadcast: moq_net::BroadcastConsumer,
	catalog: CatalogFormat,
	args: SubscribeArgs,
}

impl Subscribe {
	pub fn new(broadcast: moq_net::BroadcastConsumer, catalog: CatalogFormat, args: SubscribeArgs) -> Self {
		Self {
			broadcast,
			catalog,
			args,
		}
	}

	pub async fn run(self) -> anyhow::Result<()> {
		match self.args.format {
			SubscribeFormat::Fmp4 => self.run_fmp4().await,
			SubscribeFormat::Mkv => self.run_mkv().await,
			SubscribeFormat::Ts => self.run_ts().await,
			SubscribeFormat::Flv => self.run_flv().await,
		}
	}

	async fn run_fmp4(self) -> anyhow::Result<()> {
		let mut stdout = tokio::io::stdout();

		// Fmp4 builds the merged init segment from the first catalog snapshot, then
		// yields moof+mdat fragments in timestamp order across tracks. The catalog
		// source honors the requested format (e.g. compressed `HangZ` or `Msf`).
		let catalog = moq_mux::catalog::Consumer::<()>::new(&self.broadcast, self.catalog)?;
		let mut fmp4 = moq_mux::container::fmp4::Export::new(self.broadcast, catalog)
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
		let catalog = moq_mux::catalog::Consumer::<()>::new(&self.broadcast, self.catalog)?;
		let mut mkv = moq_mux::container::mkv::Export::new(self.broadcast, catalog)
			.with_latency(self.args.max_latency)
			.with_fragment_duration(self.args.fragment_duration);

		while let Some(chunk) = mkv.next().await? {
			stdout.write_all(&chunk).await?;
			stdout.flush().await?;
		}

		Ok(())
	}

	async fn run_ts(self) -> anyhow::Result<()> {
		let mut stdout = tokio::io::stdout();

		// TS emits PAT/PMT then a continuous PES stream (re-emitting PAT/PMT at
		// keyframes for tune-in). Avc3/Hev1 sources pass through as Annex-B; AAC
		// is re-framed as ADTS. `fragment_duration` does not apply to TS. `with_ts`
		// selects the `mpegts` catalog extension so undecoded elementary streams
		// (SCTE-35, teletext, DVB AC-3, ...) are re-emitted verbatim on their PIDs.
		let mut ts =
			moq_mux::container::ts::Export::with_ts(self.broadcast, self.catalog)?.with_latency(self.args.max_latency);

		while let Some(frame) = ts.next().await? {
			stdout.write_all(&frame.payload).await?;
			stdout.flush().await?;
		}

		Ok(())
	}

	async fn run_flv(self) -> anyhow::Result<()> {
		let mut stdout = tokio::io::stdout();

		// FLV emits the file header plus AVC/AAC sequence headers, then one tag per
		// frame interleaved by timestamp. Avc3 sources are transcoded to avc1 shape
		// internally (synthesizing avcC from inline parameter sets). Only H.264 video
		// and AAC audio are supported; `fragment_duration` does not apply to FLV.
		let mut flv = moq_mux::container::flv::Export::with_catalog_format(self.broadcast, self.catalog)?
			.with_latency(self.args.max_latency);

		while let Some(chunk) = flv.next().await? {
			stdout.write_all(&chunk).await?;
			stdout.flush().await?;
		}

		Ok(())
	}
}
