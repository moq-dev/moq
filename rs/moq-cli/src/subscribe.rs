use anyhow::Context;
use clap::ValueEnum;
use hang::moq_lite;
use tokio::io::AsyncWriteExt;
use url::Url;

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

	/// Wait for a named broadcast to be announced on the origin.
	async fn wait_broadcast(
		consumer: &mut moq_lite::OriginConsumer,
		name: &str,
	) -> anyhow::Result<moq_lite::BroadcastConsumer> {
		loop {
			let (path, announced) = consumer
				.announced()
				.await
				.ok_or_else(|| anyhow::anyhow!("origin closed"))?;

			if let Some(broadcast) = announced {
				if path.as_ref() == name {
					return Ok(broadcast);
				}
			}
		}
	}

	pub async fn run_client(
		client: moq_native::Client,
		url: Url,
		name: String,
		args: SubscribeArgs,
	) -> anyhow::Result<()> {
		let origin = moq_lite::Origin::produce();
		let mut consumer = origin.consume();

		tracing::info!(%url, %name, "connecting to subscribe");

		let mut session = client.with_consume(origin).connect(url).await?;

		#[cfg(unix)]
		let _ = sd_notify::notify(true, &[sd_notify::NotifyState::Ready]);

		let broadcast = Self::wait_broadcast(&mut consumer, &name).await?;
		let subscribe = Self::new(broadcast, args);

		tokio::select! {
			res = subscribe.run() => res,
			res = session.closed() => res.map_err(Into::into),
			_ = tokio::signal::ctrl_c() => {
				session.close(moq_lite::Error::Cancel);
				tokio::time::sleep(std::time::Duration::from_millis(100)).await;
				Ok(())
			},
		}
	}

	pub async fn run_server(server: moq_native::Server, name: String, args: SubscribeArgs) -> anyhow::Result<()> {
		let origin = moq_lite::Origin::produce();
		let mut consumer = origin.consume();

		let mut server = server.with_consume(origin);

		#[cfg(unix)]
		let _ = sd_notify::notify(true, &[sd_notify::NotifyState::Ready]);

		let mut conn_id: u64 = 0;

		tracing::info!(addr = ?server.local_addr(), "listening for subscribe");

		tokio::select! {
			res = async {
				while let Some(session) = server.accept().await {
					let id = conn_id;
					conn_id += 1;

					tokio::spawn(async move {
						if let Err(err) = consume_session(id, session).await {
							tracing::warn!(%err, "failed to accept session");
						}
					});
				}
				Ok(())
			} => res,
			res = async {
				let broadcast = Self::wait_broadcast(&mut consumer, &name).await?;
				let subscribe = Self::new(broadcast, args);
				subscribe.run().await
			} => res,
		}
	}

	pub async fn run(self) -> anyhow::Result<()> {
		match self.args.output {
			OutputFormat::Fmp4 => self.run_fmp4().await,
		}
	}

	async fn run_fmp4(self) -> anyhow::Result<()> {
		let mut stdout = tokio::io::stdout();
		let max_latency = std::time::Duration::from_millis(self.args.max_latency);

		// Always convert to CMAF — this is a no-op for tracks already in CMAF.
		let cmaf_output = moq_lite::Broadcast::new().produce();
		let cmaf_consumer = cmaf_output.consume();
		let converter = moq_mux::convert::Fmp4::new(self.broadcast, cmaf_output);

		// The converter spawns tasks for each track and returns immediately.
		converter.run().await?;

		// Read the converted catalog.
		let catalog_track = cmaf_consumer.subscribe_track(&hang::Catalog::default_track())?;
		let mut catalog_consumer = hang::CatalogConsumer::new(catalog_track);
		let catalog = catalog_consumer.next().await?.context("empty catalog")?;

		// Build exporter from catalog (for init segment)
		let exporter = moq_mux::consumer::Fmp4::new(&catalog)?;

		// Write init segment (merged multi-track moov)
		let init = exporter.init(&catalog)?;
		stdout.write_all(&init).await?;
		stdout.flush().await?;

		// Build OrderedMuxer from all track consumers (all CMAF after conversion)
		let mut muxer_tracks = Vec::new();

		for (name, config) in &catalog.video.renditions {
			let track = cmaf_consumer.subscribe_track(&moq_lite::Track {
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
			let track = cmaf_consumer.subscribe_track(&moq_lite::Track {
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

#[tracing::instrument("session", skip_all, fields(id))]
async fn consume_session(id: u64, session: moq_native::Request) -> anyhow::Result<()> {
	let session = session.ok().await?;

	tracing::info!(id, "accepted consume session");

	session.closed().await.map_err(Into::into)
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
