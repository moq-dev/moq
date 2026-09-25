//! Put a seeded, impaired UDP path in front of a server.
//!
//! Both directions get the same profile. The seed and profile print at start,
//! and the counters at exit, which fails if the profile never acted.

use std::{net::SocketAddr, time::Duration};

use clap::Parser;

#[derive(Parser)]
#[command(about)]
struct Args {
	/// The address clients send to.
	#[arg(long)]
	listen: SocketAddr,
	/// The address every datagram is forwarded to.
	#[arg(long)]
	target: SocketAddr,
	/// Seeds every treatment decision; random when omitted.
	#[arg(long)]
	seed: Option<u64>,
	/// The base one-way delay, e.g. `20ms`.
	#[arg(long, default_value = "0s", value_parser = humantime::parse_duration)]
	delay: Duration,
	/// The most the delay varies, uniformly either way.
	#[arg(long, default_value = "0s", value_parser = humantime::parse_duration)]
	jitter: Duration,
	/// The probability that a datagram is dropped.
	#[arg(long, default_value_t = 0.0)]
	loss: f64,
	/// The probability that a datagram skips the delay, overtaking those in flight.
	#[arg(long, default_value_t = 0.0)]
	reorder: f64,
	/// A rate limit in bits per second.
	#[arg(long)]
	rate: Option<u64>,
	/// Bytes the rate limit lets out ahead of its rate after an idle spell.
	#[arg(long, default_value_t = 1500, requires = "rate")]
	burst: u64,
	/// The longest a datagram waits for the rate limit before it is dropped.
	#[arg(long, default_value = "100ms", value_parser = humantime::parse_duration, requires = "rate")]
	queue: Duration,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
	let args = Args::parse();

	let profile = moq_shaper::Profile {
		delay: args.delay,
		jitter: args.jitter,
		loss: args.loss,
		reorder: args.reorder,
		rate: args.rate.map(|bits_per_second| moq_shaper::Rate {
			bits_per_second,
			burst: args.burst,
			queue: args.queue,
		}),
	};
	let shaper = moq_shaper::Shaper::bind(moq_shaper::Config {
		bind: args.listen,
		target: args.target,
		seed: args.seed.unwrap_or_else(rand::random),
		up: profile.clone(),
		down: profile,
	})
	.await?;

	let config = shaper.config();
	println!(
		"shaper: {} -> {}, seed {}, profile {:?}",
		shaper.addr(),
		config.target,
		config.seed,
		config.up
	);

	shutdown().await?;

	let stats = shaper.verify()?;
	println!("shaper: {stats}");
	Ok(())
}

/// Wait for Ctrl-C, or SIGTERM where there is one, which is how a harness stops a child.
async fn shutdown() -> anyhow::Result<()> {
	#[cfg(unix)]
	{
		let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
		tokio::select! {
			res = tokio::signal::ctrl_c() => res?,
			_ = term.recv() => {}
		}
	}
	#[cfg(not(unix))]
	tokio::signal::ctrl_c().await?;
	Ok(())
}
