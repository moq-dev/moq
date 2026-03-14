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
		use anyhow::Context;

		let mut stdout = tokio::io::stdout();
		let max_latency = std::time::Duration::from_millis(self.args.max_latency);

		// Read catalog to discover format
		let catalog_track = self.broadcast.subscribe_track(&hang::Catalog::default_track())?;
		let mut catalog_consumer = hang::CatalogConsumer::new(catalog_track);
		let catalog = catalog_consumer.next().await?.context("empty catalog")?;

		// Check if we need to convert to CMAF first
		let needs_convert = catalog
			.video
			.renditions
			.values()
			.any(|c| matches!(c.container, hang::catalog::Container::Legacy))
			|| catalog
				.audio
				.renditions
				.values()
				.any(|c| matches!(c.container, hang::catalog::Container::Legacy));

		let (broadcast, catalog) = if needs_convert {
			// Convert hang→CMAF
			let converter = moq_mux::convert::Fmp4::new(self.broadcast);
			let (broadcast, catalog_producer) = converter.run().await?;
			let catalog = catalog_producer.snapshot();
			(broadcast.consume(), catalog)
		} else {
			(self.broadcast, catalog)
		};

		// Build exporter from catalog (for init segment)
		let exporter = moq_mux::consumer::Fmp4::new(&catalog)?;

		// Write init segment (merged multi-track moov)
		let init = exporter.init(&catalog)?;
		stdout.write_all(&init).await?;
		stdout.flush().await?;

		// Build OrderedMuxer from all track consumers (all CMAF after conversion)
		let mut muxer_tracks = Vec::new();

		for (name, config) in &catalog.video.renditions {
			let track = broadcast.subscribe_track(&moq_lite::Track {
				name: name.clone(),
				priority: 1,
			})?;

			let timescale = match &config.container {
				hang::catalog::Container::Cmaf { init_data } => parse_timescale_from_init(init_data)?,
				hang::catalog::Container::Legacy => {
					anyhow::bail!("unexpected Legacy track after conversion")
				}
			};

			let consumer =
				moq_mux::consumer::OrderedConsumer::new(track, moq_mux::consumer::Cmaf { timescale }, max_latency);
			muxer_tracks.push((name.clone(), consumer));
		}

		for (name, config) in &catalog.audio.renditions {
			let track = broadcast.subscribe_track(&moq_lite::Track {
				name: name.clone(),
				priority: 2,
			})?;

			let timescale = match &config.container {
				hang::catalog::Container::Cmaf { init_data } => parse_timescale_from_init(init_data)?,
				hang::catalog::Container::Legacy => {
					anyhow::bail!("unexpected Legacy track after conversion")
				}
			};

			let consumer =
				moq_mux::consumer::OrderedConsumer::new(track, moq_mux::consumer::Cmaf { timescale }, max_latency);
			muxer_tracks.push((name.clone(), consumer));
		}

		// Use OrderedMuxer for timestamp-ordered multi-track merge
		let mut muxer = moq_mux::consumer::OrderedMuxer::new(muxer_tracks);

		while let Some(muxed) = muxer.read().await? {
			// CMAF passthrough: payload is already moof+mdat
			for chunk in &muxed.frame.payload {
				stdout.write_all(chunk).await?;
			}
			stdout.flush().await?;
		}

		Ok(())
	}
}

fn parse_timescale_from_init(init_data_b64: &str) -> anyhow::Result<u64> {
	use anyhow::Context;
	use base64::Engine;
	use mp4_atom::DecodeMaybe;

	let data = base64::engine::general_purpose::STANDARD
		.decode(init_data_b64)
		.context("invalid base64")?;
	let mut cursor = std::io::Cursor::new(&data);
	while let Some(atom) = mp4_atom::Any::decode_maybe(&mut cursor)? {
		if let mp4_atom::Any::Moov(moov) = atom {
			let trak = moov.trak.first().context("no tracks in moov")?;
			return Ok(trak.mdia.mdhd.timescale as u64);
		}
	}
	anyhow::bail!("no moov in init data")
}
