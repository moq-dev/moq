mod config;
mod connection;
mod range;
mod stats;

use std::sync::Arc;
use std::time::Duration;

pub use config::Config;
pub use range::Range;
pub use stats::Stats;

use connection::Rolled;
use rand::RngExt;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
	// TODO: It would be nice to remove this and rely on feature flags only.
	// However, some dependency is pulling in `ring` and I don't know why, so meh for now.
	rustls::crypto::aws_lc_rs::default_provider()
		.install_default()
		.expect("failed to install default crypto provider");

	let config = Config::load()?;
	anyhow::ensure!(
		config.client.connect.is_some(),
		"--client-connect is required (or set it in the TOML file)"
	);

	let config = Arc::new(config);
	let client = config.client.clone().init()?;
	let stats = Arc::new(Stats::default());

	// Periodic throughput reporter.
	{
		let stats = stats.clone();
		let interval = config.report();
		tokio::spawn(async move { stats.report(interval).await });
	}

	// Roll the per-connection parameters up front: `ThreadRng` is not `Send`, so it
	// can't cross the spawn boundary.
	let mut rng = rand::rng();
	let count = config.connections().sample(&mut rng).max(1);
	let run_id = rng.random_range(0..=u64::MAX);
	let startup = config.startup();

	tracing::info!(connections = count, url = %config.client.connect.as_ref().unwrap(), "starting benchmark");

	let mut tasks = tokio::task::JoinSet::new();
	for i in 0..count {
		let rolled = Rolled {
			broadcasts: config.broadcasts().sample(&mut rng),
			subscribe: config.subscribe().sample(&mut rng),
			fps: config.fps().sample(&mut rng),
			frame_size: config.frame_size().sample(&mut rng),
			group_size: config.group_size().sample(&mut rng),
		};

		// Stagger connection startup evenly across the ramp window.
		let delay = if count > 1 {
			startup.mul_f64(i as f64 / count as f64)
		} else {
			Duration::ZERO
		};

		let ctx = connection::Connection {
			index: i,
			run_id,
			rolled,
			config: config.clone(),
			client: client.clone(),
			stats: stats.clone(),
		};
		tasks.spawn(async move {
			tokio::time::sleep(delay).await;
			connection::run(ctx).await;
		});
	}

	let duration = config.duration;
	let stop = async move {
		match duration {
			Some(d) => tokio::time::sleep(d).await,
			None => std::future::pending::<()>().await,
		}
	};

	let drained = async { while tasks.join_next().await.is_some() {} };

	tokio::select! {
		_ = stop => tracing::info!("duration elapsed, stopping"),
		_ = tokio::signal::ctrl_c() => tracing::info!("interrupted, stopping"),
		_ = drained => tracing::warn!("all connections ended"),
	}

	Ok(())
}
