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
#[derive(clap::Args, Clone)]
pub struct Args {
	/// The derivative broadcast path. Defaults to `<broadcast>/transcode.hang`.
	#[arg(long)]
	pub output: Option<String>,

	/// A ladder rung as `height:bitrate` (pixels : bits per second), repeatable,
	/// e.g. `--rung 720:2500000 --rung 360:600000`. Rungs at or above the source
	/// are dropped at runtime. Defaults to a 1080p..240p ladder.
	#[arg(long = "rung", value_parser = parse_rung)]
	pub rungs: Vec<moq_transcode::Rung>,

	/// The video encoder: `auto` (hardware first), `hardware`, `software`, or a
	/// backend name like `nvenc`.
	#[arg(long, default_value = "auto")]
	pub encoder: String,

	/// The video decoder: `auto` (hardware first), `hardware`, `software`, or a
	/// backend name like `nvdec`.
	#[arg(long, default_value = "auto")]
	pub decoder: String,

	/// Frame resize acceleration: `auto` (GPU-backed frames stay resident), `cpu`,
	/// or `gpu`.
	#[arg(long, default_value = "auto", value_parser = parse_resize_acceleration)]
	pub resize_acceleration: moq_video::resize::Acceleration,
}

/// Parse a `height:bitrate` rung, e.g. `720:2500000`.
fn parse_rung(arg: &str) -> Result<moq_transcode::Rung, String> {
	let (height, bitrate) = arg
		.split_once(':')
		.ok_or_else(|| format!("expected height:bitrate, got `{arg}`"))?;
	let height: u32 = height.parse().map_err(|e| format!("invalid height `{height}`: {e}"))?;
	let bitrate: u64 = bitrate
		.parse()
		.map_err(|e| format!("invalid bitrate `{bitrate}`: {e}"))?;
	Ok(moq_transcode::Rung::new(height, bitrate))
}

/// Parse a frame resize acceleration preference.
fn parse_resize_acceleration(arg: &str) -> Result<moq_video::resize::Acceleration, String> {
	match arg {
		"auto" => Ok(moq_video::resize::Acceleration::Auto),
		"cpu" => Ok(moq_video::resize::Acceleration::Cpu),
		"gpu" => Ok(moq_video::resize::Acceleration::Gpu),
		_ => Err(format!("expected auto, cpu, or gpu, got `{arg}`")),
	}
}

/// Run the transcoder: subscribe to the source through the relay, publish the
/// derivative back through the same session, and serve rungs until either ends.
pub async fn run(moq: MoqSide, args: Args, net: Net) -> anyhow::Result<()> {
	let source_path = moq
		.broadcast
		.clone()
		.filter(|name| !name.is_empty())
		.context("`transcode` requires the source broadcast: pass --broadcast <name>")?;
	let output_path = args
		.output
		.clone()
		.unwrap_or_else(|| format!("{source_path}/transcode.hang"));

	// Publish the derivative through one origin and consume the source through
	// another, over a single auto-reconnecting session.
	let url = moq
		.client
		.connect
		.clone()
		.context("`transcode` requires a relay: pass --client-connect <url>")?;
	let publish = moq_net::Origin::random().produce();
	// The source has to outlive a session drop for the reconnect to be worth
	// anything: without the linger it closes on the first drop and `run` exits
	// while the redial is still in flight. This is what `Client::consume` applies
	// for the same reason; the origin is wired by hand here, so it applies it too.
	let remote = moq_net::Origin::random()
		.produce()
		.with_linger(moq.client.backoff.linger());
	let mut session = net
		.client(moq.client.clone())?
		.with_publisher(&publish)
		.with_subscriber(remote.clone())
		.connect(url);

	// Wait for the first session: the origin can't route a broadcast request
	// until a connected session registers its handler.
	while !matches!(session.status().await?, moq_native::Status::Connected) {}

	// Request the source broadcast; the session subscribes upstream on demand.
	let source = remote
		.consume()
		.request_broadcast(&source_path)
		.await
		.context("source broadcast unavailable")?;

	let mut config = moq_transcode::Config::default();
	if !args.rungs.is_empty() {
		config.rungs = args.rungs.clone();
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
	config.resize.acceleration = args.resize_acceleration;
	// Reference the source renditions relatively when the output nests under
	// the source (`a/b` -> `a/b/transcode.hang` is `..`, one `..` per level);
	// otherwise the derivative catalog advertises only the rungs.
	config.source = output_path.strip_prefix(&format!("{source_path}/")).map(|rest| {
		let depth = rest.split('/').count();
		moq_net::PathRelativeOwned::from(vec![".."; depth].join("/"))
	});

	let output = publish
		.create_broadcast(&output_path, moq_net::broadcast::Route::new().with_announce(true))
		.context("failed to create the derivative broadcast")?;
	tracing::info!(source = %source_path, output = %output_path, "transcoding");

	tokio::select! {
		res = moq_transcode::run(source, output, config) => Ok(res?),
		res = session.closed() => Ok(res?),
	}
}
