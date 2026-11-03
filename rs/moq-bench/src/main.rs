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
	moq_tokio::crypto::install_default().expect("failed to install default crypto provider");

	let config = Config::load()?;
	anyhow::ensure!(
		config.client.url.is_some(),
		"--connect is required (or set it in the TOML file)"
	);

	let config = Arc::new(config);
	let client = config.client.clone().init(config.quic.clone())?;
	// Finish rolling before starting tasks or reporting, so every line has fixed targets.
	let mut rng = rand::rng();
	let count = config.connections().sample(&mut rng).max(1);
	let run_id = rng.random_range(0..=u64::MAX);
	let connections = roll(&config, run_id, count, &mut rng);
	let mut targets = stats::Targets {
		connections: count,
		..Default::default()
	};
	for (_, rolled) in &connections {
		targets.broadcasts = targets
			.broadcasts
			.checked_add(rolled.broadcasts)
			.ok_or_else(|| anyhow::anyhow!("rolled broadcast target overflow"))?;
		targets.subscriptions = targets
			.subscriptions
			.checked_add(rolled.subscribe)
			.ok_or_else(|| anyhow::anyhow!("rolled subscription target overflow"))?;
	}
	let stats = Arc::new(Stats::new(targets));

	// Periodic throughput reporter, optionally mirrored to a JSONL file. Keep the
	// handle: a failed stats write must fail the whole run (see Stats::report).
	let mut reporter = {
		let stats = stats.clone();
		let interval = config.report();
		let output = config.output.as_ref().map(std::fs::File::create).transpose()?;
		tokio::spawn(async move { stats.report(interval, output).await })
	};

	let startup = config.startup();

	tracing::info!(
		connections = count,
		url = %moq_tokio::RedactedUrl::new(config.client.url.as_ref().unwrap()),
		"starting benchmark"
	);

	let mut tasks = tokio::task::JoinSet::new();
	for (i, (role, rolled)) in connections.into_iter().enumerate() {
		let i = i as u64;
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
			kio::clock::sleep(delay).await;
			connection::run(ctx).await;
		});
	}

	let duration = config.duration.map(Into::into);
	let stop = async move {
		match duration {
			Some(d) => kio::clock::sleep(d).await,
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

	// Tell the relay the sessions ended, rather than leaving it to wait out the idle
	// timeout, so anything measured after the load sees the relay's teardown.
	tasks.shutdown().await;
	client.close().await;

	stats.ensure_delivery(config.expects_delivery())
}

/// Roll each role before any task starts; thread-local RNG never crosses a spawn.
fn roll(config: &Config, run_id: u64, count: u64, rng: &mut impl rand::RngExt) -> Vec<(Role, Rolled)> {
	let fanout = config
		.fanout()
		.map(|name| format!("{}/{run_id:08x}/{name}", config.name()));
	(0..count)
		.map(|i| {
			let role = match &fanout {
				Some(path) if i == 0 => Role::FanoutPublisher { path: path.clone() },
				Some(path) => Role::FanoutSubscriber { path: path.clone() },
				None => Role::Mesh,
			};
			let (broadcasts, subscribe) = match &role {
				Role::FanoutPublisher { .. } => (1, 0),
				Role::FanoutSubscriber { .. } => (0, 1),
				Role::Mesh => (config.broadcasts().sample(rng), config.subscribe().sample(rng)),
			};
			(
				role,
				Rolled {
					broadcasts,
					subscribe,
					fps: config.fps().sample(rng),
					frame_size: config.frame_size().sample(rng),
					group_size: config.group_size().sample(rng),
				},
			)
		})
		.collect()
}

#[cfg(test)]
mod tests {
	use super::*;
	#[test]
	fn rolls_include_fanout_roles_and_external_subscription_targets() {
		let config = Config::parse_and_merge(["moq-bench", "--connections", "4", "--fanout", "test"]).unwrap();
		let roles = roll(&config, 0, 4, &mut rand::rng());
		assert_eq!(roles.iter().map(|(_, roll)| roll.broadcasts).sum::<u64>(), 1);
		assert_eq!(roles.iter().map(|(_, roll)| roll.subscribe).sum::<u64>(), 3);
		let config =
			Config::parse_and_merge(["moq-bench", "--broadcasts", "0", "--subscribe", "9", "--fps", "0"]).unwrap();
		let roles = roll(&config, 0, 2, &mut rand::rng());
		assert_eq!(roles.iter().map(|(_, roll)| roll.broadcasts).sum::<u64>(), 0);
		assert_eq!(roles.iter().map(|(_, roll)| roll.subscribe).sum::<u64>(), 18);
	}
}
