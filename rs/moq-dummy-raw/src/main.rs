//! Publish a single raw JSON track to a relay — same shape as ``dummy_moq_joints.py`` / leader MoQ.
//!
//! Run ``moq-relay`` first, then e.g.::
//!
//!   cargo run -p moq-dummy-raw -- --url https://127.0.0.1:4443/ --name fast-dog/leader-arm-joints --tls-disable-verify
//!
//! Watch RSS on this process and ``moq-relay`` while it runs.

use std::time::Duration;

use anyhow::Context;
use bytes::Bytes;
use clap::Parser;
use moq_lite::{Origin, Track, TrackProducer};
use url::Url;

/// Same JSON size/shape as ``cell/leader_arm/dummy_moq_joints.py`` (``both`` arms).
const JOINT_PAYLOAD: Bytes = Bytes::from_static(
	br#"{"timestamp":0,"action":[0,0,0,0,0,0,50,0,0,0,0,0,0,50],"command_type":"joint","velocity_mode":"teleop","joint_interpolate_200hz":true,"arm":"both"}"#,
);

#[derive(Parser)]
#[command(version, about)]
struct Cli {
	#[command(flatten)]
	log: moq_native::Log,

	#[command(flatten)]
	client: moq_native::ClientConfig,

	/// Relay URL (``https://host:port/`` — path is usually ``/``).
	#[arg(long)]
	url: Url,

	/// Broadcast path (e.g. ``fast-dog/leader-arm-joints``).
	#[arg(long, default_value = "fast-dog/leader-arm-joints")]
	name: String,

	/// Raw track name.
	#[arg(long, default_value = "joints")]
	track: String,

	/// Frames per second.
	#[arg(long, default_value_t = 20.0)]
	rate: f64,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
	rustls::crypto::aws_lc_rs::default_provider()
		.install_default()
		.expect("failed to install default crypto provider");

	let cli = Cli::parse();
	cli.log.init();

	let mut broadcast = moq_lite::BroadcastProducer::default();
	let origin = Origin::produce();
	origin.publish_broadcast(&cli.name, broadcast.consume());

	let _catalog = moq_mux::CatalogProducer::new(&mut broadcast).context("catalog")?;

	let mut track = broadcast
		.create_track(Track {
			name: cli.track,
			priority: 0,
		})
		.context("create_track")?;

	let client = cli.client.init()?;
	let reconnect = client.with_publish(origin.consume()).reconnect(cli.url.clone());

	tracing::info!(url = %cli.url, name = %cli.name, rate = cli.rate, "dummy raw publisher");

	let period = Duration::from_secs_f64(1.0 / cli.rate);
	let mut interval = tokio::time::interval(period);
	interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

	tokio::select! {
		res = reconnect.closed() => {
			res.context("reconnect")?;
		}
		res = pump(&mut track, &mut interval) => {
			res?;
		}
		_ = tokio::signal::ctrl_c() => {
			tracing::info!("ctrl-c");
		}
	}

	Ok(())
}

async fn pump(
	track: &mut TrackProducer,
	interval: &mut tokio::time::Interval,
) -> anyhow::Result<()> {
	loop {
		interval.tick().await;
		track
			.write_frame(JOINT_PAYLOAD.clone())
			.context("write_frame")?;
	}
}
