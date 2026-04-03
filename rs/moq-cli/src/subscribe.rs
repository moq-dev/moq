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
		// Always convert to CMAF — this is a no-op for tracks already in CMAF.
		let cmaf_output = moq_lite::Broadcast::new().produce();
		let cmaf_consumer = cmaf_output.consume();
		let converter = moq_mux::cmaf::Convert::new(self.broadcast, cmaf_output);

		// Subscribe to the catalog before the converter starts, so we don't miss it.
		let catalog_track =
			cmaf_consumer.subscribe_track(&hang::catalog::default_track(), moq_lite::Subscription::default())?;

		let max_latency = std::time::Duration::from_millis(self.args.max_latency);

		// Run the converter concurrently — it blocks until all tracks finish,
		// so we must read from the output broadcast in parallel.
		tokio::try_join!(converter.run(), mux_fmp4(catalog_track, cmaf_consumer, max_latency))?;

		Ok(())
	}
}

async fn mux_fmp4(
	catalog_track: moq_lite::TrackSubscriber,
	cmaf_consumer: moq_lite::BroadcastConsumer,
	max_latency: std::time::Duration,
) -> anyhow::Result<()> {
	let mut stdout = tokio::io::stdout();

	let mut catalog_consumer = hang::CatalogConsumer::new(catalog_track);
	let video_section = catalog_consumer.reader().section(&hang::catalog::VIDEO);
	let audio_section = catalog_consumer.reader().section(&hang::catalog::AUDIO);

	// Run until first catalog update
	tokio::select! {
		res = catalog_consumer.run() => { res?; anyhow::bail!("catalog closed before first update"); },
		_ = video_section.changed() => {},
		_ = audio_section.changed() => {},
	}

	let video = video_section.get()?.unwrap_or_default();
	let audio = audio_section.get()?.unwrap_or_default();

	// Build exporter from catalog sections (for init segment)
	let exporter = moq_mux::consumer::Fmp4::new(&video, &audio)?;

	// Write init segment (merged multi-track moov)
	let init = exporter.init(&video, &audio)?;
	stdout.write_all(&init).await?;
	stdout.flush().await?;

	// Build OrderedMuxer from all track consumers (all CMAF after conversion)
	let mut muxer_tracks = Vec::new();

	for (name, config) in &video.renditions {
		let track = cmaf_consumer.subscribe_track(
			&moq_lite::Track { name: name.clone() },
			moq_lite::Subscription::default(),
		)?;

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

	for (name, config) in &audio.renditions {
		let track = cmaf_consumer.subscribe_track(
			&moq_lite::Track { name: name.clone() },
			moq_lite::Subscription::default(),
		)?;

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

fn parse_timescale_from_init(init_data_b64: &str) -> anyhow::Result<u64> {
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
