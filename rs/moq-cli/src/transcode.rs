//! The `transcode` verb: consume a source broadcast and publish a just-in-time
//! transcoded ladder next to it.
//!
//! The derivative appears at `<broadcast>/transcode.hang` (or `--output`): its
//! catalog references the source renditions directly and adds the lower rungs,
//! which are only decoded and encoded while someone watches (or fetches) them.
//! On an NVIDIA GPU the whole pipeline is GPU-resident (NVDEC -> CUDA resize ->
//! NVENC); otherwise it falls back to software codecs.

use anyhow::Context;

use crate::Net;
use crate::args::MoqSide;
use hang::moq_net;

/// Ladder and codec options for the `transcode` verb.
#[derive(usage::Args, Clone)]
#[usage(unknown_flags = "error", args_override_self = false)]
pub struct Args {
	/// The derivative broadcast path. Defaults to `<broadcast>/transcode.hang`.
	#[usage(long)]
	pub output: Option<String>,

	/// A ladder rung as `height:bitrate` (pixels : bits per second), repeatable,
	/// e.g. `--rung 720:2500000 --rung 360:600000`. Rungs at or above the source
	/// are dropped at runtime. Defaults to a 1080p..240p ladder.
	#[usage(long = "rung")]
	rungs: Vec<RungArg>,

	/// The video encoder: `auto` (hardware first), `hardware`, `software`, or a
	/// backend name like `nvenc`.
	#[usage(long, default = "auto")]
	pub encoder: String,

	/// The video decoder: `auto` (hardware first), `hardware`, `software`, or a
	/// backend name like `nvdec`.
	#[usage(long, default = "auto")]
	pub decoder: String,

	/// Frame resize acceleration: `auto` (GPU-backed frames stay resident), `cpu`,
	/// or `gpu`.
	#[usage(long, default = "auto")]
	resize_acceleration: AccelerationArg,
}

/// A `height:bitrate` rung, e.g. `720:2500000`.
///
/// A newtype because Usage builds a value through `FromStr`, and `Rung` is not ours.
#[derive(Clone)]
struct RungArg(moq_transcode::Rung);

impl std::str::FromStr for RungArg {
	type Err = String;

	fn from_str(arg: &str) -> Result<Self, Self::Err> {
		let (height, bitrate) = arg
			.split_once(':')
			.ok_or_else(|| format!("expected height:bitrate, got `{arg}`"))?;
		let height: u32 = height.parse().map_err(|e| format!("invalid height `{height}`: {e}"))?;
		let bitrate: u64 = bitrate
			.parse()
			.map_err(|e| format!("invalid bitrate `{bitrate}`: {e}"))?;
		Ok(Self(moq_transcode::Rung::new(
			height,
			moq_net::bandwidth::Rate::from_bps(bitrate),
		)))
	}
}

#[derive(Clone, Copy)]
struct AccelerationArg(moq_video::resize::Acceleration);

impl std::str::FromStr for AccelerationArg {
	type Err = String;

	fn from_str(arg: &str) -> Result<Self, Self::Err> {
		match arg {
			"auto" => Ok(Self(moq_video::resize::Acceleration::Auto)),
			"cpu" => Ok(Self(moq_video::resize::Acceleration::Cpu)),
			"gpu" => Ok(Self(moq_video::resize::Acceleration::Gpu)),
			_ => Err(format!("expected auto, cpu, or gpu, got `{arg}`")),
		}
	}
}

/// Run the transcoder: subscribe to the source through the relay, publish the
/// derivative back through the same session, and serve rungs until either ends.
pub async fn run(moq: MoqSide, args: Args, net: Net) -> anyhow::Result<()> {
	let source_path = moq_net::PathOwned::from(
		moq.broadcast
			.clone()
			.context("`transcode` requires the source broadcast: pass --broadcast <name>")?,
	);
	if source_path.is_empty() {
		anyhow::bail!("`transcode` requires the source broadcast: pass --broadcast <name>");
	}
	let output_path = moq_net::PathOwned::from(
		args.output
			.clone()
			.unwrap_or_else(|| format!("{source_path}/transcode.hang")),
	);

	// Publish the derivative through one origin and consume the source through
	// another, over a single auto-reconnecting session.
	let url = moq
		.client
		.url
		.clone()
		.context("`transcode` requires a relay: pass --connect <url>")?;
	let publish = moq_tokio::origin::spawn(moq_net::Hop::random());
	// A session drop closes the source broadcast and ends the run: the outage is
	// surfaced rather than transcoded over. The reconnect loop covers the dial;
	// restarting after a mid-run drop is the caller's call.
	let remote = moq_tokio::origin::spawn(moq_net::Hop::random());
	let session = net
		.client(moq.client.clone())?
		.with_publisher(&publish)
		.with_subscriber(remote.clone())
		.connect(url);

	// Wait for the source to be announced rather than for the session to connect:
	// `request_broadcast` answers on the spot, so asking the moment a session exists
	// races the announcement that makes the path routable.
	//
	// Raced against the session ending, since the wait itself never fails: the origin
	// outlives the session here, so a rejected token or an exhausted retry budget would
	// otherwise leave us waiting for an announcement that can never arrive.
	let consumer = remote.consume();
	tokio::select! {
		announced = consumer.announced_broadcast(&source_path) => {
			announced.context("origin closed before the source broadcast was announced")?;
		}
		closed = session.closed() => {
			closed.context("session failed before the source broadcast was announced")?;
			anyhow::bail!("session closed before the source broadcast was announced");
		}
	}

	// Resolve it for real; the session subscribes upstream on demand.
	let source = consumer
		.request_broadcast(&source_path)
		.await
		.context("source broadcast unavailable")?;

	let mut config = moq_transcode::Config::default();
	if !args.rungs.is_empty() {
		config.rungs = args.rungs.into_iter().map(|rung| rung.0).collect();
	}
	config.encoder = match args.encoder.as_str() {
		"auto" => moq_video::encode::Kind::Auto,
		"hardware" => moq_video::encode::Kind::Hardware,
		"software" => moq_video::encode::Kind::Software,
		name => moq_video::encode::Kind::Named(name.to_string()),
	};
	config.decoder = match args.decoder.as_str() {
		"auto" => moq_video::decode::Kind::Auto,
		"hardware" => moq_video::decode::Kind::Hardware,
		"software" => moq_video::decode::Kind::Software,
		name => moq_video::decode::Kind::Named(name.to_string()),
	};
	config.resize.acceleration = args.resize_acceleration.0;
	// Point the derivative catalog at the source renditions so players fetch them from the
	// source directly. An empty reference would name the derivative broadcast itself, which
	// publishes the rungs and nothing else.
	config.source = source_path.relative(&output_path).filter(|rel| !rel.is_empty());

	let output = publish
		.create_broadcast(&output_path, moq_net::broadcast::Route::new().with_announce(true))
		.context("failed to create the derivative broadcast")?;
	tracing::info!(source = %source_path, output = %output_path, "transcoding");

	tokio::select! {
		res = moq_transcode::run(source, output, config) => Ok(res?),
		res = session.closed() => Ok(res?),
	}
}
