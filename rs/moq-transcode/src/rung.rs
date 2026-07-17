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
use crate::feed::{Feed, Item};

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
	/// The source media track, for group fetches (not yet subscribed).
	pub source: moq_net::track::Consumer,
	/// The shared live decode of the source, for the live path.
	pub feed: Feed,
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

	/// An encoder producing this rung's rendition.
	fn encode(&self) -> Result<moq_video::encode::Encoder, Error> {
		let mut config = moq_video::encode::Config::new(self.info.width, self.info.height, self.info.framerate);
		config.bitrate = Some(self.info.bitrate);
		config.kind = self.encoder.clone();
		// Keyframes are forced at every group boundary; the GOP is only a
		// backstop against pathologically long source groups.
		config.gop = self.info.framerate.saturating_mul(8).max(1);
		Ok(moq_video::encode::Encoder::new(&config)?)
	}
}

/// Serve one requested rung track until it closes or the source ends.
pub(crate) async fn serve(rung: Rung, request: moq_net::track::Request) -> Result<(), Error> {
	// Grab the group-request handle before accepting: a Request is dynamic from
	// birth, so a fetch racing the acceptance queues instead of failing.
	let dynamic = request.dynamic();
	let info = hang::container::track_info();
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

/// The live path: wait for demand, attach to the shared decode [`Feed`], and
/// resize + encode its frames group for group until demand goes away. The
/// heavy lifting (subscription, decode) is shared with every other active rung
/// of this source; only the per-rung resize and encode happen here.
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

		// One listener + encoder per demand session: rate control persists
		// across groups, while every group still opens with a forced IDR.
		// Dropping them on unused releases the shared decode (if last) and the
		// encoder session until someone subscribes again.
		let mut listener = rung.feed.listen();
		let mut encoder = rung.encode()?;

		// The output group currently being written, if the feed is mid-group.
		let mut current: Option<moq_net::group::Producer> = None;
		// Whether the next frame opens its output group (forced IDR).
		let mut first = true;

		'session: loop {
			let item = tokio::select! {
				item = listener.recv() => item,
				_ = demand.unused() => {
					if let Some(mut output) = current.take() {
						// Signal downstream that the group is incomplete.
						output.abort(moq_net::Error::Cancel)?;
					}
					break 'session;
				}
			};

			match item {
				Some(Item::Group(sequence)) => {
					if let Some(mut output) = current.take() {
						// A group boundary without an end: treat as incomplete.
						output.abort(moq_net::Error::Cancel)?;
					}
					first = true;
					// Mirror the source sequence so fetches and rendition
					// switches map 1:1.
					let info = moq_net::group::Info { sequence };
					current = match producer.create_group(info) {
						Ok(output) => Some(output),
						// A fetch task is already serving this sequence (a consumer
						// fetched a group at the live edge before the live loop
						// reached it). The fetch is authoritative and its group
						// reaches every subscriber through the shared track cache,
						// so skip it here. Residual: if that fetch then fails and
						// aborts the group, this rung skips one GOP until the next
						// keyframe. Unifying live + fetch into one cache-backed
						// serving loop (like the relay) would remove the two-writer
						// race entirely; tracked as a follow-up.
						Err(moq_net::Error::Duplicate) => None,
						Err(err) => return Err(err.into()),
					};
				}
				Some(Item::Frame(frame)) => {
					// No open group: attached mid-group, skipped a duplicate, or
					// recovering from a lag. Wait for the next boundary.
					let Some(output) = &mut current else { continue };

					let keyframe = first;
					first = false;
					// The feed decodes at the source's native size; size this
					// rung's copy here. A GPU frame resizes on the GPU and feeds
					// the encoder without touching the CPU.
					let encoded = if (frame.width, frame.height) == (rung.info.width, rung.info.height) {
						encoder.encode(&frame, keyframe)?
					} else {
						let scaled = frame.resize(rung.info.width, rung.info.height)?;
						encoder.encode(&scaled, keyframe)?
					};
					let timestamp = frame.timestamp;
					write(output, encoded.into_iter().map(|packet| (timestamp, packet)).collect())?;
				}
				Some(Item::End) => {
					if let Some(mut output) = current.take() {
						output.finish()?;
					}
				}
				Some(Item::Lagged) => {
					// Fell behind the feed: abandon the group and resume at the
					// next boundary rather than stalling other rungs.
					if let Some(mut output) = current.take() {
						output.abort(moq_net::Error::Cancel)?;
					}
				}
				Some(Item::Finished) => {
					// The source track ended: the derivative ends with it.
					if let Some(mut output) = current.take() {
						output.abort(moq_net::Error::Cancel)?;
					}
					producer.finish()?;
					return Ok(());
				}
				None => {
					// The feed died mid-stream (source or decode error).
					if let Some(mut output) = current.take() {
						let _ = output.abort(moq_net::Error::Cancel);
					}
					producer.abort(moq_net::Error::Cancel)?;
					return Ok(());
				}
			}
		}
		// listener and encoder drop here, releasing the shared decode session
		// (when this was the last rung) and the encoder.
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
	transcode_group(&mut pipeline, &container, &mut source, &mut output).await?;
	Ok(())
}

/// Transcode one fetched source group to completion into one output group,
/// draining the encoder at the end. (The live path rides the shared feed
/// instead; see [`live`].)
async fn transcode_group(
	pipeline: &mut Pipeline,
	container: &moq_mux::catalog::hang::Container,
	source: &mut moq_net::group::Consumer,
	output: &mut moq_net::group::Producer,
) -> Result<(), Error> {
	match transcode_group_inner(pipeline, container, source, output).await {
		Ok(()) => {
			output.finish()?;
			Ok(())
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
) -> Result<(), Error> {
	let mut first = true;
	// The latest presentation time seen, tracked so a one-shot group can stamp any
	// packets the encoder still holds at the end. `None` until the first frame:
	// `Timestamp` compares by raw value at a fixed scale, so there's no scale-neutral
	// zero to seed it with (the source's scale isn't known until a frame arrives).
	let mut last_timestamp: Option<moq_net::Timestamp> = None;

	while let Some(frames) = container.read(source).await? {
		for frame in frames {
			let timestamp = frame.timestamp;
			// All source frames share one scale, so this `max` never crosses scales.
			last_timestamp = Some(last_timestamp.map_or(timestamp, |last| last.max(timestamp)));

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

	if let Some(last_timestamp) = last_timestamp {
		// One-shot group: drain whatever the encoder still buffers, stamping it with
		// the last presentation time we saw. No frames read means nothing to drain.
		write(output, pipeline.finish(last_timestamp)?)?;
	}
	Ok(())
}

/// Append encoded packets to the output group in the legacy hang framing.
fn write(output: &mut moq_net::group::Producer, packets: Vec<(moq_net::Timestamp, Bytes)>) -> Result<(), Error> {
	for (timestamp, payload) in packets {
		let frame = hang::container::Frame { timestamp, payload };
		frame.write_to(output)?;
	}
	Ok(())
}

/// Decode -> resize -> encode for one fetched group of one rung.
///
/// The decoder is asked to emit frames at the rung's resolution
/// (`decode::Config::resize`). A decoder with a hardware scaler (NVDEC) does,
/// and its GPU frames feed the encoder in place: the NVDEC -> NVENC path never
/// touches the CPU. Frames that come back at any other size (software decode,
/// or a hardware decoder without a scaler) get `Frame::resize` instead.
struct Pipeline {
	decoder: moq_video::decode::Decoder,
	encoder: moq_video::encode::Encoder,
	width: u32,
	height: u32,
}

impl Pipeline {
	fn new(rung: &Rung) -> Result<Self, Error> {
		let mut decode = moq_video::decode::Config::new();
		decode.kind = rung.decoder.clone();
		decode.resize = Some((rung.info.width, rung.info.height));
		let decoder = moq_video::decode::Decoder::new(&rung.config, &decode)?;

		Ok(Self {
			decoder,
			encoder: rung.encode()?,
			width: rung.info.width,
			height: rung.info.height,
		})
	}

	/// Transcode one container payload into zero or more encoded packets, each
	/// paired with its presentation timestamp.
	fn process(
		&mut self,
		payload: &Bytes,
		timestamp: moq_net::Timestamp,
		keyframe: bool,
	) -> Result<Vec<(moq_net::Timestamp, Bytes)>, Error> {
		let mut packets = Vec::new();
		for raw in self.decoder.decode(payload, timestamp, keyframe)? {
			let raw_timestamp = raw.timestamp;
			let encoded = if (raw.width, raw.height) == (self.width, self.height) {
				// Already at the rung size (the decoder scaled): feed the frame
				// through as-is, keeping a GPU frame on the GPU.
				self.encoder.encode(&raw, keyframe)?
			} else {
				self.encoder.encode(&raw.resize(self.width, self.height)?, keyframe)?
			};
			for packet in encoded {
				packets.push((raw_timestamp, packet));
			}
		}
		Ok(packets)
	}

	/// Drain the encoder, pairing any buffered packets with `timestamp`.
	fn finish(&mut self, timestamp: moq_net::Timestamp) -> Result<Vec<(moq_net::Timestamp, Bytes)>, Error> {
		Ok(self
			.encoder
			.finish()?
			.into_iter()
			.map(|packet| (timestamp, packet))
			.collect())
	}
}
