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

		// Reject multi-track catalogs until concurrent multiplexing is implemented
		let total_tracks = catalog.video.renditions.len() + catalog.audio.renditions.len();
		anyhow::ensure!(
			total_tracks <= 1,
			"multi-track fMP4 export is not yet supported ({total_tracks} tracks found); \
			 concurrent track multiplexing to stdout requires interleaving which is not implemented"
		);

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

		// Build exporter from catalog
		let mut exporter = moq_mux::export::Fmp4::new(&catalog)?;

		// Write init segment
		let init = exporter.init(&catalog)?;
		stdout.write_all(&init).await?;
		stdout.flush().await?;

		// For each track, create an OrderedConsumer and stream frames
		// For simplicity, handle single video + single audio track
		let mut consumers = Vec::new();

		for (name, config) in &catalog.video.renditions {
			let track = broadcast.subscribe_track(&moq_lite::Track {
				name: name.clone(),
				priority: 1,
			})?;

			match &config.container {
				hang::catalog::Container::Cmaf { .. } => {
					// CMAF frames are already moof+mdat, write directly
					consumers.push((name.clone(), track, true));
				}
				hang::catalog::Container::Legacy => {
					consumers.push((name.clone(), track, false));
				}
			}
		}

		for (name, config) in &catalog.audio.renditions {
			let track = broadcast.subscribe_track(&moq_lite::Track {
				name: name.clone(),
				priority: 2,
			})?;

			match &config.container {
				hang::catalog::Container::Cmaf { .. } => {
					consumers.push((name.clone(), track, true));
				}
				hang::catalog::Container::Legacy => {
					consumers.push((name.clone(), track, false));
				}
			}
		}

		// Simple single-track streaming for now
		// TODO: multiplex multiple tracks
		for (name, track, is_cmaf) in consumers {
			if is_cmaf {
				// CMAF passthrough: frames are already moof+mdat
				// For CMAF, the frames are raw moof+mdat, not hang-encoded.
				// OrderedConsumer expects hang timestamp prefix, so we read raw instead.

				// Re-subscribe for raw access
				let track = broadcast.subscribe_track(&moq_lite::Track {
					name: name.clone(),
					priority: 1,
				})?;

				let mut track = track;
				while let Some(group) = track.next_group().await? {
					let mut reader = group;
					while let Some(data) = reader.read_frame().await? {
						stdout.write_all(&data).await?;
						stdout.flush().await?;
					}
				}
			} else {
				// Legacy: use OrderedConsumer + exporter
				let mut consumer = hang::container::OrderedConsumer::new(track, max_latency);
				while let Some(frame) = consumer.read().await? {
					let data = exporter.frame(&name, &frame)?;
					stdout.write_all(&data).await?;
					stdout.flush().await?;
				}
			}
		}

		Ok(())
	}
}
