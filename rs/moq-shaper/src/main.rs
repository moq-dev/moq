//! Put a seeded, impaired UDP path in front of a server.
//!
//! Both directions get the same profile from the flags, unless `--profile`
//! names a profile file, which sets each direction on its own. The seed and
//! profile print at start, and the counters at exit, which fails if the profile
//! never acted.

use std::{net::SocketAddr, path::PathBuf, time::Duration};

use anyhow::Context;
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
	/// Seeds every treatment decision; the profile's own, or random, when omitted.
	#[arg(long)]
	seed: Option<u64>,
	/// A built-in profile's name, or a profile TOML file, in place of the flags
	/// that shape the path.
	#[arg(
		long,
		conflicts_with_all = [
			"delay", "jitter", "jitter_model", "loss", "reorder", "rate", "burst", "queue", "batch", "batch_window",
			"shared",
		],
	)]
	profile: Option<String>,
	/// The base one-way delay, e.g. `20ms`.
	#[arg(long, default_value = "0s", value_parser = humantime::parse_duration)]
	delay: Duration,
	/// How far the delay varies: the most either way, or the sigma of a gaussian.
	#[arg(long, default_value = "0s", value_parser = humantime::parse_duration)]
	jitter: Duration,
	/// How the jitter is drawn.
	#[arg(long, value_enum, default_value_t = JitterModel::Uniform)]
	jitter_model: JitterModel,
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
	/// Hold datagrams until this many are waiting, then release them together.
	#[arg(long, requires = "batch_window")]
	batch: Option<usize>,
	/// The longest a batch waits to fill before it leaves anyway.
	#[arg(long, value_parser = humantime::parse_duration, requires = "batch")]
	batch_window: Option<Duration>,
	/// Every client shares one link each way, instead of each getting its own.
	#[arg(long)]
	shared: bool,
	/// Also pipe TCP on the listening port to the target, untouched.
	#[arg(long)]
	tcp_passthrough: bool,
	/// Write the profile, seed and counters to this file as JSON at exit.
	#[arg(long)]
	report: Option<PathBuf>,
	/// Print the same JSON as one line on stdout this often.
	#[arg(long, value_parser = humantime::parse_duration)]
	report_interval: Option<Duration>,
}

#[derive(Clone, Copy, clap::ValueEnum)]
enum JitterModel {
	/// Uniform either way of the delay, per datagram, so datagrams can overtake each other.
	Uniform,
	/// A gaussian clamped at zero that never lets a datagram overtake the one in front.
	Gaussian,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
	let args = Args::parse();

	let (name, mut setup) = match &args.profile {
		Some(profile) => {
			let preset = moq_shaper::Preset::load(profile)?;
			let mut setup = preset.setup(args.listen, args.target);
			if let Some(seed) = args.seed {
				setup.config.seed = seed;
			}
			(Some(preset.name), setup)
		}
		None => (None, flags(&args)),
	};
	setup.tcp_passthrough = args.tcp_passthrough;

	let treatment = match &name {
		Some(name) => format!("profile {name}"),
		None if setup.shared || setup.up != moq_shaper::Options::default() => format!(
			"profile {:?}, options {:?}, shared {}",
			setup.config.up, setup.up, setup.shared
		),
		None => format!("profile {:?}", setup.config.up),
	};
	let shaper = moq_shaper::Shaper::bind(setup).await?;

	let config = shaper.config();
	println!(
		"shaper: {} -> {}, seed {}, {treatment}",
		shaper.addr(),
		config.target,
		config.seed
	);

	let report = || Report {
		profile: name.as_deref(),
		seed: config.seed,
		stats: shaper.stats(),
	};

	// The first tick is a full period in, so a line reports traffic, not the start.
	let mut interval = args
		.report_interval
		.map(|period| tokio::time::interval_at(tokio::time::Instant::now() + period, period));
	let shutdown = shutdown();
	tokio::pin!(shutdown);
	loop {
		tokio::select! {
			res = &mut shutdown => {
				res?;
				break;
			}
			_ = tick(&mut interval) => println!("{}", serde_json::to_string(&report())?),
		}
	}

	// Before the verdict, so a run that fails it still leaves its counters behind.
	if let Some(path) = &args.report {
		std::fs::write(path, serde_json::to_vec(&report())?).with_context(|| format!("write {}", path.display()))?;
	}

	let stats = shaper.verify()?;
	println!("shaper: {stats}");
	Ok(())
}

/// The setup the flags describe: one profile and one set of options, both ways.
fn flags(args: &Args) -> moq_shaper::Setup {
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
	let config = moq_shaper::Config {
		bind: args.listen,
		target: args.target,
		seed: args.seed.unwrap_or_else(rand::random),
		up: profile.clone(),
		down: profile,
	};
	let options = moq_shaper::Options {
		jitter_model: match args.jitter_model {
			JitterModel::Uniform => moq_shaper::Jitter::Uniform,
			JitterModel::Gaussian => moq_shaper::Jitter::Gaussian,
		},
		batch: args
			.batch
			.zip(args.batch_window)
			.map(|(count, window)| moq_shaper::Batch { count, window }),
		..Default::default()
	};
	moq_shaper::Setup {
		shared: args.shared,
		up: options.clone(),
		down: options,
		..config.into()
	}
}

/// What a run did, as the JSON a harness reads back.
#[derive(serde::Serialize)]
struct Report<'a> {
	/// The profile's name, or null when the flags built it.
	profile: Option<&'a str>,
	seed: u64,
	#[serde(flatten)]
	stats: moq_shaper::Stats,
}

/// The next report tick, or never without an interval.
async fn tick(interval: &mut Option<tokio::time::Interval>) {
	match interval {
		Some(interval) => {
			interval.tick().await;
		}
		None => std::future::pending().await,
	}
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
