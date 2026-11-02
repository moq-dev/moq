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
#[derive(usage_rs::Args, Clone)]
#[usage(unknown_flags = "error", args_override_self = false)]
pub struct Args {
	/// The derivative broadcast path. Defaults to `<broadcast>/transcode.hang`.
	#[usage(long)]
	pub output: Option<String>,

	/// A ladder rung as `size:bitrate` (short-side pixels : bits per second), repeatable,
	/// e.g. `--rung 720:2500000 --rung 360:600000`. Short sides and bitrates must
	/// increase together; input order does not matter. Rungs above the source
	/// are dropped; a same-size rung needs a lower known bitrate.
	/// Defaults to a 240p..1080p ladder.
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

	/// Where decoded frames live: `native` (GPU-backed frames stay resident from
	/// decode through encode) or `cpu` (decode to CPU pixels, resize on the CPU).
	#[usage(long, default = "native")]
	frames: OutputArg,
}

/// A `size:bitrate` rung, e.g. `720:2500000`.
///
/// A newtype because Usage builds a value through `FromStr`, and `Rung` is not ours.
#[derive(Clone)]
struct RungArg(moq_transcode::Rung);

impl std::str::FromStr for RungArg {
	type Err = String;

	fn from_str(arg: &str) -> Result<Self, Self::Err> {
		let (size, bitrate) = arg
			.split_once(':')
			.ok_or_else(|| format!("expected size:bitrate, got `{arg}`"))?;
		let size: u32 = size.parse().map_err(|e| format!("invalid short side `{size}`: {e}"))?;
		let bitrate: u64 = bitrate
			.parse()
			.map_err(|e| format!("invalid bitrate `{bitrate}`: {e}"))?;
		Ok(Self(moq_transcode::Rung::new(
			size,
			moq_net::bandwidth::Rate::from_bps(bitrate),
		)))
	}
}

#[derive(Clone, Copy)]
struct OutputArg(moq_video::Output);

impl std::str::FromStr for OutputArg {
	type Err = String;

	fn from_str(arg: &str) -> Result<Self, Self::Err> {
		match arg {
			"native" => Ok(Self(moq_video::Output::Native)),
			"cpu" => Ok(Self(moq_video::Output::Cpu)),
			_ => Err(format!("expected native or cpu, got `{arg}`")),
		}
	}
}

/// Run the transcoder: subscribe to the source through the relay, publish the
/// derivative back through the same session, and serve rungs until either ends.
pub async fn run(moq: MoqSide, args: Args, net: Net) -> anyhow::Result<()> {
	let mut config = moq_transcode::Config::default();
	if !args.rungs.is_empty() {
		config.ladder =
			moq_transcode::Ladder::new(args.rungs.iter().map(|rung| rung.0)).context("invalid --rung ladder")?;
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
	config.resize.output = args.frames.0;

	let source_path = moq_net::Path::try_from(
		moq.broadcast
			.clone()
			.context("`transcode` requires the source broadcast: pass --broadcast <name>")?,
	)?;
	if source_path.is_empty() {
		anyhow::bail!("`transcode` requires the source broadcast: pass --broadcast <name>");
	}
	let output_path = moq_net::Path::try_from(
		args.output
			.clone()
			.unwrap_or_else(|| format!("{source_path}/transcode.hang")),
	)?;

	// Publish the derivative through one origin and consume the source through
	// another, over a single auto-reconnecting session.
	let url = moq
		.client
		.url
		.clone()
		.context("`transcode` requires a relay: pass --connect <url>")?;
	let publish = moq_tokio::origin::spawn();
	let remote = moq_tokio::origin::spawn();
	let session = net
		.client(moq.client.clone())?
		.with_publisher(&publish)
		.with_subscriber(remote.clone())
		.connect(url);

	// Point the derivative catalog at the source renditions so players fetch them from the
	// source directly. An empty reference would name the derivative broadcast itself, which
	// publishes the rungs and nothing else.
	let source = source_path
		.relative(&output_path)
		.with_context(|| format!("`{output_path}` cannot reference the source broadcast `{source_path}`"))?;
	config.source = Some(source).filter(|rel| !rel.is_empty());

	let following = Following {
		origin: remote.consume(),
		publish,
		source_path,
		output_path,
		config,
	};
	tokio::select! {
		res = following.run() => res,
		res = session.closed() => Ok(res?),
	}
}

/// Each source instance owns a fresh derivative, so its timestamps and track names never
/// describe another publisher's content on an existing broadcast.
struct Following {
	origin: moq_net::origin::Consumer,
	publish: moq_net::origin::Producer,
	source_path: moq_net::Path,
	output_path: moq_net::Path,
	config: moq_transcode::Config,
}

impl Following {
	async fn run(self) -> anyhow::Result<()> {
		use moq_mux::follow::{Event, Instances};

		let mut instances = Instances::new(&self.origin, &self.source_path, None)?;
		let mut running = None;
		loop {
			tokio::select! {
				event = instances.next() => match event? {
					Event::Start(source) => {
						// Dropping the old ladder cancels its pending requests and rung tasks.
						drop(running.take());
						let output = self.publish.create_broadcast(&self.output_path)?;
						let transcoder = moq_transcode::Transcoder::new(source, output.broadcast().clone(), self.config.clone())?;
						output.announce(moq_net::origin::Route::default().with_epoch(moq_net::Epoch::mint()))?;
						tracing::info!(source = %self.source_path, output = %self.output_path, "transcoding");
						running = Some(Box::pin(transcoder.run()));
					}
					Event::End => tracing::info!(source = %self.source_path, "offline, waiting for it to return"),
				},
				result = async { running.as_mut().expect("guarded").await }, if running.is_some() => {
					running = None;
					match result {
						Ok(()) => {}
						// The instance ended cleanly before advertising anything to transcode.
						Err(moq_transcode::Error::NoSource) => {
							tracing::info!(source = %self.source_path, "ended before its video was ready, waiting for it to return");
						}
						Err(err) => return Err(err).context("transcoding failed"),
					}
				}
			}
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	/// A pending source catalog is a real run even before an encoder is needed. Replacing
	/// that source must retire the old derivative and announce a fresh instance immediately.
	#[tokio::test(start_paused = true)]
	async fn a_source_restart_replaces_the_derivative_while_its_catalog_is_pending() {
		restart(true).await;
	}

	#[tokio::test(start_paused = true)]
	async fn an_epochless_source_restart_also_creates_a_new_derivative() {
		restart(false).await;
	}

	async fn restart(with_epoch: bool) {
		let origin = moq_tokio::origin::spawn();
		let publish = moq_tokio::origin::spawn();
		let route = || {
			let route = moq_net::origin::Route::default();
			if with_epoch {
				route.with_epoch(moq_net::Epoch::mint())
			} else {
				route
			}
		};
		let old = origin.publish("source", route()).unwrap();
		let _old_dynamic = old.requests();
		let mut derivatives = moq_mux::follow::Instances::new(&publish.consume(), "derived", None).unwrap();
		let following = Following {
			origin: origin.consume(),
			publish: publish.clone(),
			source_path: hang::moq_net::Path::from_static("source"),
			output_path: hang::moq_net::Path::from_static("derived"),
			config: Default::default(),
		};
		let task = tokio::spawn(following.run());
		let first = next(&mut derivatives).await;
		let mut old_catalog = first
			.track(hang::Catalog::DEFAULT_NAME)
			.unwrap()
			.subscribe(None)
			.await
			.unwrap();
		let new = origin.publish("source", route()).unwrap();
		let _new_dynamic = new.requests();
		let second = next(&mut derivatives).await;
		assert!(!first.is_clone(&second));
		assert_ne!(first.info().epoch, second.info().epoch);
		assert!(first.is_closed(), "the superseded ladder is retired");
		loop {
			match old_catalog.recv_group().await {
				Ok(Some(_)) => continue,
				Err(moq_net::Error::Cancel) => break,
				_ => panic!("the retired catalog must report deliberate cancellation"),
			}
		}
		assert!(!second.is_closed(), "the new ladder serves immediately");
		assert!(
			!old.consume().is_closed(),
			"replacement does not close the source publisher"
		);
		task.abort();
		assert!(task.await.unwrap_err().is_cancelled());
	}

	/// A source that finishes its catalog before advertising any video is a clean end, so the
	/// command keeps following and the next instance still gets a ladder.
	#[tokio::test(start_paused = true)]
	async fn a_source_ending_before_its_video_waits_for_the_next_instance() {
		let origin = moq_tokio::origin::spawn();
		let publish = moq_tokio::origin::spawn();
		let route = || moq_net::origin::Route::default().with_epoch(moq_net::Epoch::mint());
		let old = origin.publish("source", route()).unwrap();
		let mut catalog = moq_mux::catalog::Producer::new(&mut old.broadcast().clone(), Default::default()).unwrap();
		let mut derivatives = moq_mux::follow::Instances::new(&publish.consume(), "derived", None).unwrap();
		let following = Following {
			origin: origin.consume(),
			publish: publish.clone(),
			source_path: hang::moq_net::Path::from_static("source"),
			output_path: hang::moq_net::Path::from_static("derived"),
			config: Default::default(),
		};
		let task = tokio::spawn(following.run());
		let first = next(&mut derivatives).await;

		catalog.finish().unwrap();
		kio::clock::timeout(std::time::Duration::from_secs(1), first.closed())
			.await
			.expect("the ladder outlived its source's end");
		assert!(!task.is_finished(), "a clean end stopped following");

		let new = origin.publish("source", route()).unwrap();
		let _new_dynamic = new.requests();
		let second = next(&mut derivatives).await;
		assert!(!first.is_clone(&second));
		task.abort();
		assert!(task.await.unwrap_err().is_cancelled());
	}

	/// The next derivative instance announced at the output path.
	async fn next(instances: &mut moq_mux::follow::Instances) -> moq_net::broadcast::Consumer {
		let start = async {
			loop {
				match instances.next().await.unwrap() {
					moq_mux::follow::Event::Start(broadcast) => return broadcast,
					moq_mux::follow::Event::End => continue,
				}
			}
		};
		kio::clock::timeout(std::time::Duration::from_secs(1), start)
			.await
			.expect("no derivative instance was announced")
	}
}
