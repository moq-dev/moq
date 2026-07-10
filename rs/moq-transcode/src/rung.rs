//! Per-rung serving: just-in-time encoding of one output rendition.
//!
//! Nothing is encoded until someone asks, via the two demand paths moq-net
//! exposes on the output track:
//!
//! - A live subscription (`used`) starts a live loop that subscribes to the
//!   source track (mirroring the aggregate subscription) and transcodes group
//!   for group until the track goes `unused` again.
//! - A fetch of a specific group (`requested_group`) fetches that same group
//!   from the source and transcodes just that group with a fresh encoder.
//!
//! Output groups mirror the source group sequence numbers 1:1, so a fetch for
//! output group N maps to source group N and a player switching renditions
//! lands on the same content.

use std::sync::Arc;

use bytes::Bytes;
use hang::catalog::VideoConfig;
use moq_mux::container::Container as _;
use tokio::sync::Semaphore;

use crate::Error;
use crate::catalog::Resolved;
use crate::scale::Scaler;

/// Cap on transcode pipelines a single rung builds concurrently for on-demand
/// group fetches. Each pipeline holds a decoder + encoder session, and hardware
/// encoders expose only a few simultaneous sessions, so an unbounded fetch burst
/// (a rendition-switching player requesting many past groups at once) would
/// exhaust them and fail live viewers too. Global admission across rungs and
/// nodes is the fleet's concern; this is the local backstop.
const MAX_CONCURRENT_FETCHES: usize = 4;

/// Everything a rung needs to build transcoding pipelines on demand.
#[derive(Clone)]
pub(crate) struct Rung {
	pub info: Resolved,
	/// The source media track (not yet subscribed; demand drives that).
	pub source: moq_net::track::Consumer,
	/// The source broadcast, to notice it closing while idle.
	pub broadcast: moq_net::broadcast::Consumer,
	/// The source rendition's catalog entry (codec + container).
	pub config: VideoConfig,
	/// Which encoder implementation to use.
	pub encoder: moq_video::encode::Kind,
	/// Which decoder implementation to use.
	pub decoder: moq_video::decode::Kind,
}

impl Rung {
	fn pipeline(&self) -> Result<Pipeline, Error> {
		Pipeline::new(self)
	}

	fn container(&self) -> Result<moq_mux::catalog::hang::Container, Error> {
		Ok(moq_mux::catalog::hang::Container::try_from(&self.config.container)?)
	}
}

/// Serve one requested rung track until it closes or the source ends.
pub(crate) async fn serve(rung: Rung, request: moq_net::track::Request) -> Result<(), Error> {
	// Grab the group-request handle before accepting: a Request is dynamic from
	// birth, so a fetch racing the acceptance queues instead of failing.
	let dynamic = request.dynamic();
	let info = moq_net::track::Info::default().with_timescale(hang::container::TIMESCALE);
	let mut producer = request.accept(info);

	let result = tokio::select! {
		res = live(&rung, &mut producer) => res,
		res = fetches(&rung, &dynamic) => res,
	};
	if result.is_err() {
		// End the track so subscribers see an error rather than a stall.
		let _ = producer.abort(moq_net::Error::Cancel);
	}
	result
}

/// The live path: wait for demand, mirror the aggregate subscription upstream,
/// and transcode group for group until demand goes away.
async fn live(rung: &Rung, producer: &mut moq_net::track::Producer) -> Result<(), Error> {
	let demand = producer.demand();
	loop {
		tokio::select! {
			used = demand.used() => if used.is_err() {
				// The output track closed; nothing more to serve.
				return Ok(());
			},
			err = rung.broadcast.closed() => {
				// The source went away while idle; end the rung with it.
				producer.abort(err)?;
				return Ok(());
			}
		}

		// Mirror the downstream demand upstream (priority, ordering, start).
		let subscription = producer.subscription().unwrap_or_default();
		let mut subscriber = rung.source.subscribe(subscription).await?;

		// One pipeline per demand session: rate control persists across groups,
		// while every group still opens with a forced IDR.
		let mut pipeline = rung.pipeline()?;
		let container = rung.container()?;

		'session: loop {
			tokio::select! {
				group = subscriber.next_group() => {
					let Some(mut source) = group? else {
						// The source track ended: the derivative ends with it.
						producer.finish()?;
						return Ok(());
					};
					// Mirror the source sequence so fetches and rendition
					// switches map 1:1.
					let info = moq_net::group::Info { sequence: source.sequence };
					let mut output = match producer.create_group(info) {
						Ok(output) => output,
						// A fetch task is already serving this sequence (a consumer
						// fetched a group at the live edge before the live loop
						// reached it). The fetch is authoritative and its group
						// reaches every subscriber through the shared track cache,
						// so skip it here. Residual: if that fetch then fails and
						// aborts the group, this rung skips one GOP until the next
						// keyframe. Unifying live + fetch into one cache-backed
						// serving loop (like the relay) would remove the two-writer
						// race entirely; tracked as a follow-up.
						Err(moq_net::Error::Duplicate) => continue,
						Err(err) => return Err(err.into()),
					};
					let done = transcode_group(&mut pipeline, &container, &mut source, &mut output, Some(&demand)).await?;
					if !done {
						// Demand disappeared mid-group; back to waiting.
						break 'session;
					}
				}
				_ = demand.unused() => break 'session,
			}
		}
		// Dropping the subscriber releases the upstream subscription (and the
		// encoder) until someone subscribes again.
	}
}

/// The fetch path: serve requests for specific (past) groups.
///
/// Fetch tasks run under a local [`JoinSet`](tokio::task::JoinSet) rather than
/// detached: when `serve` cancels this future (the live path ended, or the
/// output track closed), dropping the set aborts every in-flight fetch, so none
/// keep a source subscription or an encoder session alive past teardown. A
/// semaphore bounds how many run at once.
async fn fetches(rung: &Rung, dynamic: &moq_net::track::Dynamic) -> Result<(), Error> {
	let limit = Arc::new(Semaphore::new(MAX_CONCURRENT_FETCHES));
	let mut tasks = tokio::task::JoinSet::new();

	loop {
		// Reap finished fetches so the set doesn't grow without bound.
		while tasks.try_join_next().is_some() {}

		let Ok(request) = dynamic.requested_group().await else {
			// The output track closed; nothing more to serve.
			return Ok(());
		};

		// Take a slot before spawning the transcode. Under a burst this blocks
		// here, so further requests queue in the dynamic handler (backpressure)
		// instead of spawning unbounded pipelines. The semaphore is never closed,
		// so acquire only fails if we drop it first.
		let Ok(permit) = limit.clone().acquire_owned().await else {
			return Ok(());
		};

		let rung = rung.clone();
		tasks.spawn(async move {
			let _permit = permit;
			let sequence = request.sequence();
			if let Err(err) = fetch(rung, request).await {
				tracing::warn!(%err, sequence, "transcode fetch failed");
			}
		});
	}
}

/// Transcode one specifically requested group, fetching it from the source.
///
/// Every early exit rejects the request with a real error: dropping a
/// `GroupRequest` auto-rejects with [`moq_net::Error::Dropped`], which reads as
/// "the handler vanished" and hides the actual decode/encode/source failure from
/// the waiting consumer.
async fn fetch(rung: Rung, request: moq_net::track::GroupRequest) -> Result<(), Error> {
	let options = moq_net::group::Fetch::default().with_priority(request.priority());
	let mut source = match rung.source.fetch_group(request.sequence(), options).await {
		Ok(source) => source,
		Err(err) => {
			request.reject(err.clone());
			return Err(err.into());
		}
	};

	// A fresh pipeline per fetched group: groups are independently decodable,
	// so the encoder starts clean at the group's keyframe.
	let (mut pipeline, container) = match rung.pipeline().and_then(|p| rung.container().map(|c| (p, c))) {
		Ok(built) => built,
		Err(err) => {
			request.reject(moq_net::Error::Cancel);
			return Err(err);
		}
	};

	let mut output = match request.accept(None) {
		Ok(output) => output,
		Err(err) => return Err(err.into()),
	};
	transcode_group(&mut pipeline, &container, &mut source, &mut output, None).await?;
	Ok(())
}

/// Transcode one source group into one output group.
///
/// With a `demand` handle (the live path) the group is abandoned as soon as the
/// track goes unused, returning `Ok(false)`; without one (the fetch path) the
/// group always runs to completion and the encoder is drained at the end.
async fn transcode_group(
	pipeline: &mut Pipeline,
	container: &moq_mux::catalog::hang::Container,
	source: &mut moq_net::group::Consumer,
	output: &mut moq_net::group::Producer,
	demand: Option<&moq_net::track::Demand>,
) -> Result<bool, Error> {
	match transcode_group_inner(pipeline, container, source, output, demand).await {
		Ok(done) => {
			if done {
				output.finish()?;
			} else {
				// Demand disappeared mid-group: signal downstream that the
				// group is incomplete rather than leaving it short-but-finished.
				output.abort(moq_net::Error::Cancel)?;
			}
			Ok(done)
		}
		Err(err) => {
			let _ = output.abort(moq_net::Error::Cancel);
			Err(err)
		}
	}
}

async fn transcode_group_inner(
	pipeline: &mut Pipeline,
	container: &moq_mux::catalog::hang::Container,
	source: &mut moq_net::group::Consumer,
	output: &mut moq_net::group::Producer,
	demand: Option<&moq_net::track::Demand>,
) -> Result<bool, Error> {
	let mut first = true;
	let mut last_timestamp = 0u64;

	loop {
		let frames = match demand {
			Some(demand) => tokio::select! {
				frames = container.read(source) => frames,
				_ = demand.unused() => return Ok(false),
			},
			None => container.read(source).await,
		};
		let Some(frames) = frames? else {
			break;
		};

		for frame in frames {
			let timestamp: u64 = frame
				.timestamp
				.as_micros()
				.try_into()
				.map_err(|_| moq_net::TimeOverflow)?;
			last_timestamp = last_timestamp.max(timestamp);

			// A group opens on a keyframe by construction, so the first frame is
			// an IDR. The low-level `Container::read` the transcoder uses does not
			// reconstruct the keyframe bit for legacy sources (that lives in the
			// higher-level container consumer), so `first` is the reliable signal;
			// OR in the container's own flag so CMAF mid-group keyframes still
			// force an output IDR. This flag drives both the decoder (keyframe
			// gating + parameter-set injection) and the encoder (forced IDR).
			let keyframe = frame.keyframe || first;
			first = false;

			write(output, pipeline.process(&frame.payload, timestamp, keyframe)?)?;
		}
	}

	if demand.is_none() {
		// One-shot group: drain whatever the encoder still buffers.
		write(output, pipeline.finish(last_timestamp)?)?;
	}
	Ok(true)
}

/// Append encoded packets to the output group in the legacy hang framing.
fn write(output: &mut moq_net::group::Producer, packets: Vec<(u64, Bytes)>) -> Result<(), Error> {
	for (timestamp, payload) in packets {
		let frame = hang::container::Frame {
			timestamp: moq_net::Timestamp::from_micros(timestamp)?,
			payload,
		};
		frame.encode(output)?;
	}
	Ok(())
}

/// Decode -> scale -> encode for one rung.
struct Pipeline {
	decoder: moq_video::decode::Decoder,
	scaler: Scaler,
	encoder: moq_video::encode::Encoder,
	width: u32,
	height: u32,
}

impl Pipeline {
	fn new(rung: &Rung) -> Result<Self, Error> {
		let decoder = moq_video::decode::Decoder::new(&rung.config, &rung.decoder)?;

		let mut config = moq_video::encode::Config::new(rung.info.width, rung.info.height, rung.info.framerate);
		config.bitrate = Some(rung.info.bitrate);
		config.kind = rung.encoder.clone();
		// Keyframes are forced at every group boundary; the GOP is only a
		// backstop against pathologically long source groups.
		config.gop = rung.info.framerate.saturating_mul(8).max(1);
		let encoder = moq_video::encode::Encoder::new(&config)?;

		Ok(Self {
			decoder,
			scaler: Scaler::new(rung.info.width, rung.info.height),
			encoder,
			width: rung.info.width,
			height: rung.info.height,
		})
	}

	/// Transcode one container payload into zero or more encoded packets, each
	/// paired with its presentation timestamp in microseconds.
	fn process(&mut self, payload: &Bytes, timestamp: u64, keyframe: bool) -> Result<Vec<(u64, Bytes)>, Error> {
		let mut packets = Vec::new();
		for raw in self.decoder.decode(payload, timestamp, keyframe)? {
			let scaled = self.scaler.scale(&raw.data, raw.width, raw.height)?;
			for packet in self.encoder.encode_i420(scaled, self.width, self.height, keyframe)? {
				packets.push((raw.timestamp_us, packet));
			}
		}
		Ok(packets)
	}

	/// Drain the encoder, pairing any buffered packets with `timestamp`.
	fn finish(&mut self, timestamp: u64) -> Result<Vec<(u64, Bytes)>, Error> {
		Ok(self
			.encoder
			.finish()?
			.into_iter()
			.map(|packet| (timestamp, packet))
			.collect())
	}
}
