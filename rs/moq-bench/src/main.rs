mod config;
mod connection;
mod range;
mod stats;

use std::sync::Arc;
use std::time::Duration;

pub use config::Config;
pub use range::Range;
pub use stats::Stats;

use connection::{Role, Rolled};
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
		config.client.url.is_some(),
		"--connect is required (or set it in the TOML file)"
	);

	let config = Arc::new(config);
	let client = config.client.clone().init(config.quic.clone())?;
	let stats = Arc::new(Stats::default());

	// Periodic throughput reporter, optionally mirrored to a JSONL file. Keep the
	// handle: a failed stats write must fail the whole run (see Stats::report).
	let mut reporter = {
		let stats = stats.clone();
		let interval = config.report();
		let output = config.output.as_ref().map(std::fs::File::create).transpose()?;
		tokio::spawn(async move { stats.report(interval, output).await })
	};

	// Roll the per-connection parameters up front: `ThreadRng` is not `Send`, so it
	// can't cross the spawn boundary.
	let mut rng = rand::rng();
	let count = config.connections().sample(&mut rng).max(1);
	let run_id = rng.random_range(0..=u64::MAX);
	let startup = config.startup();
	let fanout = config
		.fanout()
		.map(|name| format!("{}/{run_id:08x}/{name}", config.name()));

	tracing::info!(
		connections = count,
		url = %moq_tokio::RedactedUrl::new(config.client.url.as_ref().unwrap()),
		"starting benchmark"
	);

	let mut tasks = tokio::task::JoinSet::new();
	for i in 0..count {
		let role = match &fanout {
			Some(path) if i == 0 => Role::FanoutPublisher { path: path.clone() },
			Some(path) => Role::FanoutSubscriber { path: path.clone() },
			None => Role::Mesh,
		};
		let (broadcasts, subscribe) = match &role {
			Role::FanoutPublisher { .. } => (1, 0),
			Role::FanoutSubscriber { .. } => (0, 1),
			Role::Mesh => (
				config.broadcasts().sample(&mut rng),
				config.subscribe().sample(&mut rng),
			),
		};
		let rolled = Rolled {
			broadcasts,
			subscribe,
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
			role,
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

	let duration = config.duration.map(moq_tokio::Duration::into_std);
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
		_ = drained => anyhow::bail!("all benchmark connections ended"),
		// The reporter only returns on a stats-output failure; die loudly rather
		// than exit green with a partial JSONL file.
		res = &mut reporter => res??,
	}

	stats.ensure_delivery(config.expects_delivery())
}
