mod args;
mod hls;
mod moq;
mod publish;
mod rtc;
mod rtmp;
mod srt;
mod subscribe;
mod web;

use args::{Cli, Direction, Export, ExportSink, Import, ImportSource};
use hang::moq_net;
use publish::Publish;
use subscribe::{Subscribe, SubscribeArgs};

use clap::Parser;
use tokio::task::JoinSet;

/// Everything needed to build MoQ clients/servers, encapsulating the optional
/// iroh endpoint so the rest of the code is feature-agnostic.
#[derive(Clone)]
struct Net {
	#[cfg(feature = "iroh")]
	iroh: Option<moq_native::iroh::Endpoint>,
}

impl Net {
	fn client(&self, config: moq_native::ClientConfig) -> anyhow::Result<moq_native::Client> {
		let client = config.init()?;
		#[cfg(feature = "iroh")]
		let client = client.with_iroh(self.iroh.clone());
		Ok(client)
	}

	fn server(&self, config: moq_native::ServerConfig) -> anyhow::Result<moq_native::Server> {
		let server = config.init()?;
		#[cfg(feature = "iroh")]
		let server = server.with_iroh(self.iroh.clone());
		Ok(server)
	}
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
	// TODO: It would be nice to remove this and rely on feature flags only.
	// However, some dependency is pulling in `ring` and I don't know why, so meh for now.
	rustls::crypto::aws_lc_rs::default_provider()
		.install_default()
		.expect("failed to install default crypto provider");

	let cli = Cli::parse();
	cli.log.init()?;

	let net = Net {
		#[cfg(feature = "iroh")]
		iroh: cli.iroh.bind().await?,
	};

	match cli.direction {
		Direction::Import(import) => run_import(import, net).await,
		Direction::Export(export) => run_export(export, net).await,
	}
}

/// Route one source INTO the shared Origin, exposing it to the MoQ network.
async fn run_import(import: Import, net: Net) -> anyhow::Result<()> {
	let Import { moq, source } = import;

	let origin = moq_net::Origin::random().produce();
	// The broadcast defaults to "": MoQ names each broadcast by the connection
	// path plus any explicit `--broadcast`, so an unset name is the root broadcast.
	let name = moq.broadcast_name().unwrap_or_default();
	let mut tasks: JoinSet<anyhow::Result<()>> = JoinSet::new();

	// MoQ side: publish the Origin outward.
	if let Some(url) = moq.client_url() {
		let client = net.client(moq.client)?;
		let origin = origin.clone();
		tasks.spawn(async move { moq::client_import(client, url, &origin).await });
	}
	if moq.server.bind.is_some() {
		let web_bind = moq.server.bind.clone().expect("server bind present");
		let server = net.server(moq.server)?;
		let tls_info = server.tls_info();
		let dir = moq.dir;
		tasks.spawn(moq::server_import(server, origin.clone()));
		tasks.spawn(async move { web::run_web(&web_bind, tls_info, dir).await });
	}

	// Foreign side: the single source.
	match source {
		ImportSource::Stdin { format } => {
			warn_if_missing_format(&name);
			let publish = Publish::new(&format)?;
			anyhow::ensure!(
				origin.publish_broadcast(&name, publish.consume()),
				"failed to publish broadcast"
			);
			tasks.spawn(async move { publish.run().await });
		}
		ImportSource::Hls { connect } => {
			warn_if_missing_format(&name);
			let origin = origin.clone();
			tasks.spawn(async move { hls::import(&origin, name, connect).await });
		}
		ImportSource::Rtmp(rtmp) => {
			if let Some(addr) = rtmp.listen {
				tasks.spawn(rtmp::listen_import(origin.clone(), addr, rtmp.prefix));
			} else if let Some(url) = rtmp.connect {
				tasks.spawn(rtmp::connect_import(origin.clone(), url, name));
			}
		}
		ImportSource::Srt(srt) => {
			if let Some(addr) = srt.listen {
				tasks.spawn(srt::listen_import(origin.clone(), addr, srt.prefix, srt.latency));
			} else if let Some(url) = srt.connect {
				tasks.spawn(srt::connect_import(origin.clone(), url, name, srt.latency));
			}
		}
		ImportSource::Rtc(rtc) => {
			if let Some(addr) = rtc.listen {
				tasks.spawn(rtc::listen_import(origin.clone(), addr, rtc.udp_bind, rtc.public_addr));
			} else if let Some(url) = rtc.connect {
				tasks.spawn(rtc::connect_import(origin.clone(), url, name));
			}
		}
	}

	drive(tasks).await
}

/// Route the shared Origin OUT to one sink, filling it from the MoQ network.
async fn run_export(export: Export, net: Net) -> anyhow::Result<()> {
	let Export { moq, sink } = export;

	let origin = moq_net::Origin::random().produce();
	// The broadcast defaults to "": MoQ names each broadcast by the connection
	// path plus any explicit `--broadcast`, so an unset name is the root broadcast.
	let name = moq.broadcast_name().unwrap_or_default();
	let mut tasks: JoinSet<anyhow::Result<()>> = JoinSet::new();

	// MoQ side: fill the Origin.
	if let Some(url) = moq.client_url() {
		let client = net.client(moq.client)?;
		let origin = origin.clone();
		tasks.spawn(async move { moq::client_export(client, url, origin).await });
	}
	if moq.server.bind.is_some() {
		let web_bind = moq.server.bind.clone().expect("server bind present");
		let server = net.server(moq.server)?;
		let tls_info = server.tls_info();
		let dir = moq.dir;
		tasks.spawn(moq::server_export(server, origin.clone()));
		tasks.spawn(async move { web::run_web(&web_bind, tls_info, dir).await });
	}

	// Foreign side: the single sink.
	match sink {
		ExportSink::Stdout {
			format,
			max_latency,
			fragment_duration,
			catalog,
		} => {
			let args = SubscribeArgs {
				format,
				max_latency,
				fragment_duration,
				catalog,
			};
			let consumer = origin.consume();
			tasks.spawn(async move { run_stdout(consumer, name, args).await });
		}
		ExportSink::Hls {
			listen,
			tls,
			part_target,
			window,
		} => {
			tasks.spawn(hls::export(origin.consume(), listen, tls, part_target, window));
		}
		ExportSink::Rtmp(rtmp) => {
			if let Some(addr) = rtmp.listen {
				tasks.spawn(rtmp::listen_export(origin.consume(), addr, rtmp.prefix));
			} else if let Some(url) = rtmp.connect {
				tasks.spawn(rtmp::connect_export(origin.consume(), url, name));
			}
		}
		ExportSink::Srt(srt) => {
			if let Some(addr) = srt.listen {
				tasks.spawn(srt::listen_export(origin.consume(), addr, srt.prefix, srt.latency));
			} else if let Some(url) = srt.connect {
				tasks.spawn(srt::connect_export(origin.consume(), url, name, srt.latency));
			}
		}
		ExportSink::Rtc(rtc) => {
			if let Some(addr) = rtc.listen {
				tasks.spawn(rtc::listen_export(
					origin.consume(),
					addr,
					rtc.udp_bind,
					rtc.public_addr,
				));
			} else if let Some(url) = rtc.connect {
				tasks.spawn(rtc::connect_export(origin.consume(), url, name));
			}
		}
	}

	drive(tasks).await
}

/// Subscribe to `name` from the Origin and write it to stdout.
async fn run_stdout(consumer: moq_net::OriginConsumer, name: String, args: SubscribeArgs) -> anyhow::Result<()> {
	let catalog = args.catalog_format(&name);
	let broadcast = consumer
		.announced_broadcast(&name)
		.await
		.ok_or_else(|| anyhow::anyhow!("origin closed before broadcast `{name}` was announced"))?;

	Subscribe::new(broadcast, catalog, args).run().await
}

/// Run every endpoint until the first finishes (stdin EOF, Ctrl-C, or an error),
/// then drop the rest.
async fn drive(mut tasks: JoinSet<anyhow::Result<()>>) -> anyhow::Result<()> {
	tasks.spawn(async {
		let _ = tokio::signal::ctrl_c().await;
		Ok(())
	});

	while let Some(res) = tasks.join_next().await {
		match res {
			Ok(Ok(())) => return Ok(()),
			Ok(Err(err)) => return Err(err),
			Err(err) if err.is_cancelled() => continue,
			Err(err) => return Err(err.into()),
		}
	}

	Ok(())
}

fn warn_if_missing_format(name: &str) {
	// The empty (root) broadcast has no name to suffix, so there's nothing to warn about.
	if !name.is_empty() && moq_mux::catalog::CatalogFormat::detect(name).is_none() {
		tracing::warn!(
			name,
			"You should append .hang to your broadcast name to make the catalog format explicit."
		);
	}
}
