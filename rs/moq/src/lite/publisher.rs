use std::sync::Arc;

use web_async::FuturesExt;
use web_transport_trait::SendStream;

use crate::{
	coding::{Stream, Writer},
	lite,
	model::GroupConsumer,
	AsPath, BroadcastConsumer, Error, Origin, OriginConsumer, Track, TrackConsumer,
};

pub(super) struct Publisher<S: web_transport_trait::Session> {
	session: S,
	origin: OriginConsumer,
}

impl<S: web_transport_trait::Session> Publisher<S> {
	pub fn new(session: S, origin: Option<OriginConsumer>) -> Self {
		// Default to a dummy origin that is immediately closed.
		let origin = origin.unwrap_or_else(|| Origin::produce().consumer);
		Self { session, origin }
	}

	pub async fn run(mut self) -> Result<(), Error> {
		loop {
			let mut stream = tokio::select! {
				res = Stream::accept(&self.session) => res?,
				// Discard updates: each ANNOUNCE_PLEASE gets its own consumer seeded with the active set.
				// Otherwise this unbounded queue pins every (un)announced broadcast until the session closes.
				Some(_) = self.origin.announced() => continue,
			};

			// To avoid cloning the origin, we process each control stream in received order.
			// This adds some head-of-line blocking but it delays an expensive clone.
			let kind = stream.reader.decode().await?;

			if let Err(err) = match kind {
				lite::ControlType::Announce => self.recv_announce(stream).await,
				lite::ControlType::Subscribe => self.recv_subscribe(stream).await,
				_ => Err(Error::UnexpectedStream),
			} {
				tracing::warn!(%err, "control stream error");
			}
		}
	}

	pub async fn recv_announce(&mut self, mut stream: Stream<S>) -> Result<(), Error> {
		let interest = stream.reader.decode::<lite::AnnouncePlease>().await?;
		let prefix = interest.prefix.to_owned();

		// For logging, show the full path that we're announcing.
		tracing::trace!(root = %self.origin.absolute(&prefix), "announcing start");

		let mut origin = self
			.origin
			.consume_only(&[prefix.as_path()])
			.ok_or(Error::Unauthorized)?;

		web_async::spawn(async move {
			if let Err(err) = Self::run_announce(&mut stream, &mut origin, &prefix).await {
				match &err {
					Error::Cancel => {
						tracing::debug!(prefix = %origin.absolute(prefix), "announcing cancelled");
					}
					Error::Transport(_) => {
						tracing::debug!(prefix = %origin.absolute(prefix), "announcing cancelled");
					}
					err => {
						tracing::warn!(%err, prefix = %origin.absolute(prefix), "announcing error");
					}
				}

				stream.writer.abort(&err);
			} else {
				tracing::trace!(prefix = %origin.absolute(prefix), "announcing complete");
			}
		});

		Ok(())
	}

	async fn run_announce(
		stream: &mut Stream<S>,
		origin: &mut OriginConsumer,
		prefix: impl AsPath,
	) -> Result<(), Error> {
		let prefix = prefix.as_path();
		let mut init = Vec::new();

		// Send ANNOUNCE_INIT as the first message with all currently active paths
		// We use `try_next()` to synchronously get the initial updates.
		while let Some((path, active)) = origin.try_announced() {
			let suffix = path.strip_prefix(&prefix).expect("origin returned invalid path");

			if active.is_some() {
				tracing::debug!(broadcast = %origin.absolute(&path), "announce");
				init.push(suffix.to_owned());
			} else {
				// A potential race.
				tracing::debug!(broadcast = %origin.absolute(&path), "unannounce");
				init.retain(|path| path != &suffix);
			}
		}

		let announce_init = lite::AnnounceInit { suffixes: init };
		stream.writer.encode(&announce_init).await?;

		// Flush any synchronously announced paths
		loop {
			tokio::select! {
				biased;
				res = stream.reader.closed() => return res,
				announced = origin.announced() => {
					match announced {
						Some((path, active)) => {
							let suffix = path.strip_prefix(&prefix).expect("origin returned invalid path").to_owned();

							if active.is_some() {
								tracing::debug!(broadcast = %origin.absolute(&path), "announce");
								let msg = lite::Announce::Active { suffix };
								stream.writer.encode(&msg).await?;
							} else {
								tracing::debug!(broadcast = %origin.absolute(&path), "unannounce");
								let msg = lite::Announce::Ended { suffix };
								stream.writer.encode(&msg).await?;
							}
						},
						None => return stream.writer.finish().await,
					}
				}
			}
		}
	}

	pub async fn recv_subscribe(&mut self, mut stream: Stream<S>) -> Result<(), Error> {
		let subscribe = stream.reader.decode::<lite::Subscribe>().await?;

		let id = subscribe.id;
		let track = subscribe.track.clone();
		let absolute = self.origin.absolute(&subscribe.broadcast).to_owned();

		tracing::info!(%id, broadcast = %absolute, %track, "subscribed started");

		let broadcast = self.origin.consume_broadcast(&subscribe.broadcast);

		let session = self.session.clone();
		web_async::spawn(async move {
			if let Err(err) = Self::run_subscribe(session, &mut stream, &subscribe, broadcast).await {
				match &err {
					// TODO better classify WebTransport errors.
					Error::Cancel | Error::Transport(_) => {
						tracing::info!(%id, broadcast = %absolute, %track, "subscribed cancelled")
					}
					err => {
						tracing::warn!(%id, broadcast = %absolute, %track, %err, "subscribed error")
					}
				}
				stream.writer.abort(&err);
			} else {
				tracing::info!(%id, broadcast = %absolute, %track, "subscribed complete")
			}
		});

		Ok(())
	}

	async fn run_subscribe(
		session: S,
		stream: &mut Stream<S>,
		subscribe: &lite::Subscribe<'_>,
		consumer: Option<BroadcastConsumer>,
	) -> Result<(), Error> {
		let track = Track {
			name: subscribe.track.to_string(),
			priority: subscribe.priority,
		};

		let broadcast = consumer.ok_or(Error::NotFound)?;
		let track = broadcast.subscribe_track(&track);

		// TODO wait until track.info() to get the *real* priority

		let info = lite::SubscribeOk {
			priority: track.info.priority,
		};

		stream.writer.encode(&info).await?;

		tokio::select! {
			res = Self::run_track(session, track, subscribe) => res?,
			res = stream.reader.closed() => res?,
		}

		stream.writer.finish().await
	}

	async fn run_track(session: S, mut track: TrackConsumer, subscribe: &lite::Subscribe<'_>) -> Result<(), Error> {
		// TODO use a BTreeMap serve the latest N groups by sequence.
		// Until then, we'll implement N=2 manually.
		// Also, this is more complicated because we can't use tokio because of WASM.
		// We need to drop futures in order to cancel them and keep polling them with select!
		let mut old_group = None;
		let mut new_group = None;

		// Annoying that we can't use a tuple here as we need the compiler to infer the type.
		// Otherwise we'd have to pick Send or !Send...
		let mut old_sequence = None;
		let mut new_sequence = None;

		// Keep reading groups from the track, some of which may arrive out of order.
		loop {
			let group = tokio::select! {
				biased;
				Some(group) = track.next_group().transpose() => group,
				Some(_) = async { Some(old_group.as_mut()?.await) } => {
					old_group = None;
					old_sequence = None;
					continue;
				},
				Some(_) = async { Some(new_group.as_mut()?.await) } => {
					new_group = old_group;
					new_sequence = old_sequence;
					old_group = None;
					old_sequence = None;
					continue;
				},
				else => return Ok(()),
			}?;

			let sequence = group.info.sequence;
			let latest = new_sequence.as_ref().unwrap_or(&0);

			tracing::debug!(subscribe = %subscribe.id, track = %track.info.name, sequence, latest, "serving group");

			// If this group is older than the oldest group we're serving, skip it.
			// We always serve at most two groups, but maybe we should serve only sequence >= MAX-1.
			if sequence < *old_sequence.as_ref().unwrap_or(&0) {
				tracing::debug!(subscribe = %subscribe.id, track = %track.info.name, old = %sequence, %latest, "skipping group");
				continue;
			}

			let priority = stream_priority(track.info.priority, sequence);
			let msg = lite::Group {
				subscribe: subscribe.id,
				sequence,
			};

			// Spawn a task to serve this group, ignoring any errors because they don't really matter.
			// TODO add some logging at least.
			let handle = Box::pin(Self::serve_group(session.clone(), msg, priority, group));

			// Terminate the old group if it's still running.
			if let Some(old_sequence) = old_sequence.take() {
				tracing::debug!(subscribe = %subscribe.id, track = %track.info.name, old = %old_sequence, %latest, "aborting group");
				old_group.take(); // Drop the future to cancel it.
			}

			assert!(old_group.is_none());

			if sequence >= *latest {
				old_group = new_group;
				old_sequence = new_sequence;

				new_group = Some(handle);
				new_sequence = Some(sequence);
			} else {
				old_group = Some(handle);
				old_sequence = Some(sequence);
			}
		}
	}

	async fn serve_group(session: S, msg: lite::Group, priority: i32, mut group: GroupConsumer) -> Result<(), Error> {
		// TODO add a way to open in priority order.
		let mut stream = session
			.open_uni()
			.await
			.map_err(|err| Error::Transport(Arc::new(err)))?;
		stream.set_priority(priority);

		let mut stream = Writer::new(stream);
		stream.encode(&lite::DataType::Group).await?;
		stream.encode(&msg).await?;

		loop {
			let frame = tokio::select! {
				biased;
				_ = stream.closed() => return Err(Error::Cancel),
				frame = group.next_frame() => frame,
			};

			let mut frame = match frame? {
				Some(frame) => frame,
				None => break,
			};

			tracing::trace!(size = %frame.info.size, "writing frame");

			stream.encode(&frame.info.size).await?;

			loop {
				let chunk = tokio::select! {
					biased;
					_ = stream.closed() => return Err(Error::Cancel),
					chunk = frame.read_chunk() => chunk,
				};

				match chunk? {
					Some(mut chunk) => stream.write_all(&mut chunk).await?,
					None => break,
				}
			}

			tracing::trace!(size = %frame.info.size, "wrote frame");
		}

		stream.finish().await?;

		tracing::debug!(sequence = %msg.sequence, "finished group");

		Ok(())
	}
}

// Quinn takes a i32 priority.
// We do our best to distill 70 bits of information into 32 bits, but overflows will happen.
// Specifically, group sequence 2^24 will overflow and be incorrectly prioritized.
// But even with a group per frame, it will take ~6 days to reach that point.
// TODO The behavior when two tracks share the same priority is undefined. Should we round-robin?
fn stream_priority(track_priority: u8, group_sequence: u64) -> i32 {
	let sequence = 0xFFFFFF - (group_sequence as u32 & 0xFFFFFF);
	((track_priority as i32) << 24) | sequence as i32
}

#[cfg(test)]
mod test {
	use super::*;

	#[test]
	fn priority() {
		let assert = |track_priority, group_sequence, expected| {
			assert_eq!(stream_priority(track_priority, group_sequence), expected);
		};

		const U24: i32 = (1 << 24) - 1;

		// NOTE: The lower the value, the higher the priority for Quinn.
		// MoQ does the opposite, so we invert the values.
		assert(0, 50, U24 - 50);
		assert(0, 0, U24);
		assert(1, 50, 2 * U24 - 49);
		assert(1, 0, 2 * U24 + 1);
	}
}
