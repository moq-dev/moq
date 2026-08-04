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
	/// How to resize decoded frames.
	pub resize: moq_video::resize::Config,
}

impl Rung {
	async fn pipeline(&self) -> Result<Pipeline, Error> {
		Pipeline::new(self).await
	}

	fn container(&self) -> Result<moq_mux::catalog::hang::Container, Error> {
		Ok(moq_mux::catalog::hang::Container::try_from(&self.config.container)?)
	}

	/// An encoder producing this rung's rendition.
	///
	/// `color` is the space the frames reaching it are in, taken from the first
	/// decoded frame where the decoder knows. A rung below 576 lines fed by an HD
	/// source carries the source's space, not the one its own size implies, so
	/// leaving this to the encoder's size guess would label the rung wrongly.
	///
	/// A [`Sink`](moq_video::encode::Sink) rather than a bare `Encoder`: both
	/// callers hold it across `.await`, so on a multi-thread runtime the future
	/// can migrate workers between opening the codec and dropping it. The Windows
	/// backend's COM apartment is per-thread, so that would leak the opening
	/// worker's initialization and uninitialize COM on one that never initialized
	/// it. The sink owns a thread and stays on it.
	async fn encode(&self, color: Option<moq_video::Color>) -> Result<moq_video::encode::Sink, Error> {
		let mut config =
			moq_video::encode::Config::new(self.info.size.width, self.info.size.height, self.info.framerate);
		config.bitrate = Some(self.info.bitrate);
		config.kind = self.encoder.clone();
		config.color = color;
		// Keyframes are forced at every group boundary; the GOP is only a
		// backstop against pathologically long source groups.
		config.gop = self.info.framerate.saturating_mul(8).max(1);
		Ok(moq_video::encode::Sink::open(&config).await?)
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
				producer.clone().abort(err)?;
				return Ok(());
			}
		}

		// One listener + encoder per demand session: rate control persists
		// across groups, while every group still opens with a forced IDR.
		// Dropping them on unused releases the shared decode (if last) and the
		// encoder session until someone subscribes again.
		let mut listener = rung.feed.listen();
		// Built from the first frame: the encoder writes that frame's color space
		// into the bitstream, so it cannot open before one has arrived. A keyframe
		// asked for at a group boundary waits here until it exists.
		let mut encoder: Option<moq_video::encode::Sink> = None;
		let mut pending_keyframe = false;

		// The output group currently being written, if the feed is mid-group.
		let mut current: Option<moq_net::group::Producer> = None;

		'session: loop {
			let item = tokio::select! {
				item = listener.recv() => item,
				_ = demand.unused() => {
					if let Some(output) = current.take() {
						// Signal downstream that the group is incomplete.
						output.abort(moq_net::Error::Cancel)?;
					}
					break 'session;
				}
			};

			match item {
				Some(Item::Group(sequence)) => {
					// Empty the codec before opening the next group even though this one
					// is being abandoned: a pipelined encoder still holding the previous
					// group's tail would otherwise emit it into the new group, ahead of
					// the keyframe requested just below.
					if let Some(encoder) = &mut encoder {
						encoder.flush().await?;
					}
					if let Some(output) = current.take() {
						// A group boundary without an end: treat as incomplete.
						output.abort(moq_net::Error::Cancel)?;
					}
					// A subscriber has to be able to start at this group, so its first
					// frame must be an IDR. The request waits for the next frame, so a
					// rung that skips this group simply carries it forward.
					match &mut encoder {
						Some(encoder) => encoder.keyframe(),
						None => pending_keyframe = true,
					}
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

					// The feed decodes at the source's native size; size this rung's copy
					// here. Supported GPU paths feed the encoder without touching the
					// CPU. The resize carries the source's color space across, which is
					// why the encoder is opened from the scaled frame rather than from
					// this rung's size.
					let frame: Arc<moq_video::Frame> = match frame.size() == rung.info.size {
						true => frame,
						false => Arc::new(frame.resize_with(rung.info.size, &rung.resize)?),
					};
					let encoder = match &mut encoder {
						Some(encoder) => encoder,
						None => {
							let mut opened = rung.encode(frame.surface.color()).await?;
							if std::mem::take(&mut pending_keyframe) {
								opened.keyframe();
							}
							encoder.insert(opened)
						}
					};
					write(output, encoder.encode(frame).await?)?;
				}
				Some(Item::End) => {
					if let Some(mut output) = current.take() {
						// The source group is complete, so this one has to be too: a
						// hardware encoder is still holding its last frames.
						if let Some(encoder) = &mut encoder {
							write(&mut output, encoder.flush().await?)?;
						}
						output.finish()?;
					}
				}
				Some(Item::Lagged) => {
					// Fell behind the feed: abandon the group and resume at the
					// next boundary rather than stalling other rungs.
					if let Some(output) = current.take() {
						output.abort(moq_net::Error::Cancel)?;
					}
				}
				Some(Item::Finished) => {
					// The source track ended: the derivative ends with it.
					if let Some(output) = current.take() {
						output.abort(moq_net::Error::Cancel)?;
					}
					producer.finish()?;
					return Ok(());
				}
				None => {
					// The feed died mid-stream (source or decode error).
					if let Some(output) = current.take() {
						let _ = output.abort(moq_net::Error::Cancel);
					}
					producer.clone().abort(moq_net::Error::Cancel)?;
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
	let (pipeline, container) = match rung.pipeline().await.and_then(|p| rung.container().map(|c| (p, c))) {
		Ok(built) => built,
		Err(err) => {
			request.reject(moq_net::Error::Cancel);
			return Err(err);
		}
	};

	let output = match request.accept(None) {
		Ok(output) => output,
		Err(err) => return Err(err.into()),
	};
	transcode_group(pipeline, &container, &mut source, output).await?;
	Ok(())
}

/// Transcode one fetched source group to completion into one output group,
/// draining the encoder at the end. (The live path rides the shared feed
/// instead; see [`live`].)
async fn transcode_group(
	pipeline: Pipeline,
	container: &moq_mux::catalog::hang::Container,
	source: &mut moq_net::group::Consumer,
	mut output: moq_net::group::Producer,
) -> Result<(), Error> {
	match transcode_group_inner(pipeline, container, source, &mut output).await {
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
	mut pipeline: Pipeline,
	container: &moq_mux::catalog::hang::Container,
	source: &mut moq_net::group::Consumer,
	output: &mut moq_net::group::Producer,
) -> Result<(), Error> {
	let mut first = true;

	while let Some(frames) = container.read(source).await? {
		for frame in frames {
			let timestamp = frame.timestamp;

			// A group opens on a keyframe by construction, so the first frame is
			// an IDR. The low-level `Container::read` the transcoder uses does not
			// reconstruct the keyframe bit for legacy sources (that lives in the
			// higher-level container consumer), so `first` is the reliable signal;
			// OR in the container's own flag so CMAF mid-group keyframes still
			// force an output IDR. This flag drives both the decoder (keyframe
			// gating + parameter-set injection) and the encoder (forced IDR).
			let keyframe = frame.keyframe || first;
			first = false;

			write(output, pipeline.process(frame.payload, timestamp, keyframe).await?)?;
		}
	}

	// One-shot group: drain whatever the encoder still buffers. Each packet keeps
	// the timestamp of the frame it was encoded from, so the tail stays in step.
	write(output, pipeline.finish().await?)?;
	Ok(())
}

/// Append encoded frames to the output group in the legacy hang framing.
fn write(output: &mut moq_net::group::Producer, encoded: Vec<moq_video::encode::Encoded>) -> Result<(), Error> {
	for encoded in encoded {
		let frame = hang::container::Frame {
			timestamp: encoded.timestamp,
			payload: encoded.payload,
		};
		frame.write_to(output)?;
	}
	Ok(())
}

/// Decode -> resize -> encode for one fetched group of one rung.
///
/// Unless CPU scaling is forced, the decoder is asked to emit frames at the
/// rung's resolution (`decode::Config::resize`). A decoder with a hardware
/// scaler (NVDEC) does, and its GPU frames feed the encoder in place: the NVDEC
/// -> NVENC path never touches the CPU. Frames that come back at any other size
/// get `Frame::resize_with` instead.
struct Pipeline {
	decoder: moq_video::decode::Sink,
	/// Opened from the first decoded frame, whose color space it has to declare.
	/// `None` until one arrives; a keyframe requested before then waits in
	/// `pending_keyframe`.
	encoder: Option<moq_video::encode::Sink>,
	pending_keyframe: bool,
	rung: Rung,
	size: moq_video::Size,
}

impl Pipeline {
	async fn new(rung: &Rung) -> Result<Self, Error> {
		let mut decode = moq_video::decode::Config::new();
		decode.kind = rung.decoder.clone();
		decode.resize = decoder_resize(rung.info.size, rung.resize.acceleration);
		// A `Sink` for the same reason as the encoder above: a fetch task holds this
		// pipeline across `Container::read`, so the codec must own its thread.
		let decoder = moq_video::decode::Sink::open(&rung.config, &decode).await?;

		Ok(Self {
			decoder,
			encoder: None,
			pending_keyframe: false,
			rung: rung.clone(),
			size: rung.info.size,
		})
	}

	/// Transcode one container payload into zero or more encoded frames, each
	/// carrying the presentation time of the picture it came from.
	async fn process(
		&mut self,
		payload: Bytes,
		timestamp: moq_net::Timestamp,
		keyframe: bool,
	) -> Result<Vec<moq_video::encode::Encoded>, Error> {
		// This group opens on an IDR. The encoder holds the request until a picture
		// actually arrives, which matters because a decoder that buffers returns
		// nothing for the access unit that asked for one.
		if keyframe {
			match &mut self.encoder {
				Some(encoder) => encoder.keyframe(),
				None => self.pending_keyframe = true,
			}
		}

		let mut encoded = Vec::new();
		for raw in self.decoder.decode(payload, timestamp, keyframe).await? {
			// Already at the rung size (the decoder scaled): feed the frame through
			// as-is, keeping a GPU frame on the GPU.
			let raw = match raw.size() == self.size {
				true => raw,
				false => raw.resize_with(self.size, &self.rung.resize)?,
			};
			if self.encoder.is_none() {
				let mut opened = self.rung.encode(raw.surface.color()).await?;
				if std::mem::take(&mut self.pending_keyframe) {
					opened.keyframe();
				}
				self.encoder = Some(opened);
			}
			let encoder = self.encoder.as_mut().expect("just opened");
			encoded.extend(encoder.encode(raw).await?);
		}
		Ok(encoded)
	}

	/// Drain the encoder, keeping each buffered packet's own timestamp.
	///
	/// Consumes the pipeline, since flushing the encoder consumes it: a one-shot
	/// group's pipeline is done once drained.
	async fn finish(self) -> Result<Vec<moq_video::encode::Encoded>, Error> {
		// No encoder means no frame ever decoded, so there is nothing buffered.
		match self.encoder {
			Some(encoder) => encoder.finish().await.map_err(Into::into),
			None => Ok(Vec::new()),
		}
	}
}

/// Let a hardware decoder scale only when the caller has not forced the CPU.
fn decoder_resize(size: moq_video::Size, acceleration: moq_video::resize::Acceleration) -> Option<moq_video::Size> {
	(acceleration != moq_video::resize::Acceleration::Cpu).then_some(size)
}

#[cfg(test)]
mod tests {
	use super::decoder_resize;
	use moq_video::resize::Acceleration;

	/// A fetched NVDEC group reaches `Frame::resize_with` at native size when
	/// CPU scaling is forced, rather than being resized in the decoder first.
	#[test]
	fn forced_cpu_skips_the_decoder_scaler() {
		let size = moq_video::Size::new(160, 120);

		assert_eq!(decoder_resize(size, Acceleration::Cpu), None);
		assert_eq!(decoder_resize(size, Acceleration::Auto), Some(size));
		assert_eq!(decoder_resize(size, Acceleration::Gpu), Some(size));
	}
}
