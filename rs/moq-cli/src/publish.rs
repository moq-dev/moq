use clap::ValueEnum;
use hang::moq_lite;
use moq_mux::producer;
use url::Url;

#[derive(ValueEnum, Clone, Copy)]
pub enum InputFormat {
	Fmp4,
	Avc3,
	Hls,
}

#[derive(ValueEnum, Clone, Copy)]
pub enum ExportFormat {
	Hang,
	Fmp4,
}

#[derive(clap::Args, Clone)]
pub struct PublishArgs {
	/// Input format (what's being read from stdin).
	/// For hls, provide the playlist URL on stdin.
	#[arg(long)]
	pub input: InputFormat,

	/// Convert to a different format before publishing.
	/// If not specified, publishes in the import's native format.
	#[arg(long)]
	pub export: Option<ExportFormat>,
}

enum PublishKind {
	Avc3(Box<producer::Avc3>),
	Fmp4(Box<producer::Fmp4>),
	Hls(Box<producer::Hls>),
}

pub struct Publish {
	kind: PublishKind,
	export: Option<ExportFormat>,

	/// The broadcast the importer writes into.
	import_broadcast: moq_lite::BroadcastProducer,

	/// The broadcast that gets published (after optional conversion).
	output_broadcast: moq_lite::BroadcastProducer,
}

impl Publish {
	pub fn new(args: &PublishArgs) -> anyhow::Result<Self> {
		let mut import_broadcast = moq_lite::Broadcast::new().produce();
		let catalog = moq_mux::CatalogProducer::new(&mut import_broadcast)?;

		let kind = match args.input {
			InputFormat::Avc3 => {
				let avc3 = producer::Avc3::new(import_broadcast.clone(), catalog.clone());
				PublishKind::Avc3(Box::new(avc3))
			}
			InputFormat::Fmp4 => {
				let fmp4 = producer::Fmp4::new(import_broadcast.clone(), catalog.clone());
				PublishKind::Fmp4(Box::new(fmp4))
			}
			InputFormat::Hls => {
				// Read playlist URL from stdin (first line)
				let mut playlist = String::new();
				std::io::stdin().read_line(&mut playlist)?;
				let playlist = playlist.trim().to_string();
				anyhow::ensure!(!playlist.is_empty(), "expected HLS playlist URL on stdin");

				let config = producer::HlsConfig::new(playlist);
				let hls = producer::Hls::new(import_broadcast.clone(), catalog.clone(), config)?;
				PublishKind::Hls(Box::new(hls))
			}
		};

		// If exporting, create a separate output broadcast for the converter.
		// Otherwise, the output is the same as the import.
		let output_broadcast = if args.export.is_some() {
			moq_lite::Broadcast::new().produce()
		} else {
			import_broadcast.clone()
		};

		Ok(Self {
			kind,
			export: args.export,
			import_broadcast,
			output_broadcast,
		})
	}

	pub fn consume(&self) -> moq_lite::BroadcastConsumer {
		self.output_broadcast.consume()
	}

	pub async fn run(self) -> anyhow::Result<()> {
		let Self {
			mut kind,
			export,
			import_broadcast,
			output_broadcast,
		} = self;

		let Some(export) = export else {
			return run_import(&mut kind).await;
		};

		let import_consumer = import_broadcast.consume();

		match export {
			ExportFormat::Fmp4 => {
				let converter = moq_mux::convert::Fmp4::new(import_consumer, output_broadcast);
				tokio::select! {
					res = run_import(&mut kind) => res,
					res = converter.run() => res,
				}
			}
			ExportFormat::Hang => {
				let converter = moq_mux::convert::Hang::new(import_consumer, output_broadcast);
				tokio::select! {
					res = run_import(&mut kind) => res,
					res = converter.run() => res,
				}
			}
		}
	}

	pub async fn run_client(self, client: moq_native::Client, url: Url, name: String) -> anyhow::Result<()> {
		let origin = moq_lite::Origin::produce();
		origin.publish_broadcast(&name, self.consume());

		tracing::info!(%url, %name, "connecting");

		let mut session = client.with_publish(origin.consume()).connect(url).await?;

		#[cfg(unix)]
		let _ = sd_notify::notify(true, &[sd_notify::NotifyState::Ready]);

		tokio::select! {
			res = self.run() => res,
			res = session.closed() => res.map_err(Into::into),
			_ = tokio::signal::ctrl_c() => {
				session.close(moq_lite::Error::Cancel);
				tokio::time::sleep(std::time::Duration::from_millis(100)).await;
				Ok(())
			},
		}
	}

	pub async fn run_server(self, mut server: moq_native::Server, name: String) -> anyhow::Result<()> {
		let consumer = self.consume();

		#[cfg(unix)]
		let _ = sd_notify::notify(true, &[sd_notify::NotifyState::Ready]);

		let mut conn_id: u64 = 0;

		tracing::info!(addr = ?server.local_addr(), "listening");

		tokio::select! {
			res = async {
				while let Some(session) = server.accept().await {
					let id = conn_id;
					conn_id += 1;

					let name = name.clone();
					let consumer = consumer.clone();

					tokio::spawn(async move {
						if let Err(err) = publish_session(id, session, name, consumer).await {
							tracing::warn!(%err, "failed to accept session");
						}
					});
				}
				Ok(())
			} => res,
			res = self.run() => res,
		}
	}
}

async fn run_import(kind: &mut PublishKind) -> anyhow::Result<()> {
	match kind {
		PublishKind::Avc3(decoder) => {
			let mut stdin = tokio::io::stdin();
			decoder.decode_from(&mut stdin).await
		}
		PublishKind::Fmp4(decoder) => {
			let mut stdin = tokio::io::stdin();
			decoder.decode_from(&mut stdin).await
		}
		PublishKind::Hls(hls) => {
			hls.init().await?;
			hls.run().await
		}
	}
}

#[tracing::instrument("session", skip_all, fields(id))]
async fn publish_session(
	id: u64,
	session: moq_native::Request,
	name: String,
	consumer: moq_lite::BroadcastConsumer,
) -> anyhow::Result<()> {
	let origin = moq_lite::Origin::produce();
	origin.publish_broadcast(&name, consumer);

	let session = session.with_publish(origin.consume()).ok().await?;

	tracing::info!(id, "accepted session");

	session.closed().await.map_err(Into::into)
}
