//! Consuming a JSON value from a track: a [`Decoder`] plus the track it reads from.

use std::task::Poll;

use serde::de::DeserializeOwned;

use super::Decoder;
use crate::Result;

/// Track-owning options for a [`Consumer`].
///
/// Build from [`Default`] and override fields (the struct is `#[non_exhaustive]`, so new options
/// stay additive).
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct Config {
	/// Maximum bytes in a frame or the compact JSON of the reconstructed value.
	///
	/// Unset, the group budget is the only bound: a group holds at most
	/// [`MAX_CACHE_BYTES`](moq_net::group::MAX_CACHE_BYTES), compressed or not, so the
	/// reconstructed value costs no more than that. Set it to hold a smaller document, at the cost
	/// of re-measuring the value after every delta.
	pub max_size: Option<usize>,

	/// Delivery priority of the consumer's subscription, higher first, as
	/// [`Subscription::priority`](moq_net::track::Subscription::priority).
	pub priority: i8,
}

/// Consumes a JSON value from a track, reconstructing it from snapshots and deltas.
///
/// A [`Decoder`] that owns its track: it reads groups, routes each frame by its position, and
/// yields each reconstructed state and its frame timestamp, or skips to the newest with `latest`.
/// When something else already owns the track, use the [`Decoder`]
/// directly.
///
/// A snapshot is superseded the moment a newer one exists, so the consumer subscribes itself at a
/// zero [`max`](moq_net::track::Delay::max) delay and keeps that subscription to itself: a group
/// is skipped once a newer one arrives, and a lower group the subscription still owes never holds
/// up a newer one.
pub struct Consumer<T> {
	track: Track,
	name: String,
	group: Option<moq_net::group::Consumer>,
	decoder: Decoder<T>,
	frames_read: usize,
}

impl<T: DeserializeOwned> Consumer<T> {
	/// Create a consumer reading from the given track.
	///
	/// Subscribes right away at [`Config::priority`] and a zero
	/// [`max`](moq_net::track::Delay::max) delay, with default preferences otherwise; no handle to that subscription leaves the
	/// consumer, so nothing can change its budget. A read waits until the subscription resolves,
	/// and fails if it cannot. A [`compress`](moq_net::track::Info::compress)ed track reads the
	/// same: moq-net inflates it.
	pub fn new(track: moq_net::track::Consumer, config: Config) -> Self {
		let subscription = moq_net::track::Subscription::default()
			.with_priority(config.priority)
			.with_delay(moq_net::track::Delay::default().with_max(std::time::Duration::ZERO));
		let name = track.name().to_string();
		let mut track = Track::Subscribing(track.subscribe(subscription).into_inner());
		// Join now when the track already resolves, as an in-process one does, so the publisher
		// sees the reader before its first read; otherwise the first read resolves it.
		track.resolve(&kio::Waiter::noop());
		Self {
			name,
			track,
			group: None,
			decoder: Decoder::new(config),
			frames_read: 0,
		}
	}

	/// Get the next state and its exact frame timestamp, or `None` once the track ends.
	pub async fn next(&mut self) -> Result<Option<moq_net::Timed<T>>>
	where
		T: Unpin,
	{
		kio::wait(|waiter| self.poll_next(waiter)).await
	}

	/// Poll for each state in order, skipping every group a newer one has superseded.
	///
	/// Finish the current group's buffered frames before taking the next. Lost groups
	/// resynchronize from a later snapshot.
	pub fn poll_next(&mut self, waiter: &kio::Waiter) -> Poll<Result<Option<moq_net::Timed<T>>>> {
		self.poll_read(waiter, false)
	}

	/// Skip to the newest buffered state and its timestamp, or wait for a new one.
	pub async fn latest(&mut self) -> Result<Option<moq_net::Timed<T>>>
	where
		T: Unpin,
	{
		kio::wait(|waiter| self.poll_latest(waiter)).await
	}

	/// Poll for the newest state, discarding superseded groups and intermediate yields.
	///
	/// Frames inside the newest group are still decoded in order. What this skips is gone;
	/// a subsequent `next` resumes after it. An untimed frame keeps `at: None`.
	pub fn poll_latest(&mut self, waiter: &kio::Waiter) -> Poll<Result<Option<moq_net::Timed<T>>>> {
		self.poll_read(waiter, true)
	}

	fn poll_read(&mut self, waiter: &kio::Waiter, skip: bool) -> Poll<Result<Option<moq_net::Timed<T>>>> {
		if skip {
			// Jump to the newest group the reader already has; a newer one supersedes the rest.
			// Taking groups, not the track's largest position, ignores datagrams (which share
			// the sequence space).
			while let Poll::Ready(Some(group)) = self.track.poll_next_group(waiter)? {
				self.group = Some(group);
				self.frames_read = 0;
			}
		}

		let mut advanced = None;
		let mut finished = false;
		loop {
			let Some(group) = &mut self.group else {
				match self.track.poll_next_group(waiter)? {
					Poll::Ready(Some(group)) => {
						self.group = Some(group);
						self.frames_read = 0;
						continue;
					}
					Poll::Ready(None) => {
						finished = true;
						break;
					}
					Poll::Pending => break,
				}
			};

			match group.poll_read_frame(waiter) {
				Poll::Ready(Ok(Some(frame))) => {
					self.apply(&frame.payload)?;
					if !skip {
						return Poll::Ready(Ok(self.decoder.decode()?.map(|value| moq_net::Timed {
							value,
							at: frame.timestamp,
						})));
					}
					advanced = Some(frame.timestamp);
				}
				Poll::Ready(Ok(None)) => self.group = None,
				// Judged by stream code, so a group the publisher reset as old is the same loss as
				// one this side dropped.
				Poll::Ready(Err(err))
					if matches!(
						moq_net::StreamError::from(&err),
						moq_net::StreamError::Old
							| moq_net::StreamError::Evicted
							| moq_net::StreamError::TooFarBehind
							| moq_net::StreamError::GroupTooLarge
					) =>
				{
					// A later group starts a fresh snapshot.
					tracing::warn!(track = self.name, group = group.sequence, error = ?err,
						"snapshot group lost; waiting for a newer one");
					self.group = None;
				}
				Poll::Ready(Err(err)) => return Poll::Ready(Err(err.into())),
				Poll::Pending => break,
			}
		}
		if let Some(at) = advanced {
			Poll::Ready(Ok(self.decoder.decode()?.map(|value| moq_net::Timed { value, at })))
		} else if finished {
			Poll::Ready(Ok(None))
		} else {
			Poll::Pending
		}
	}

	/// Apply one frame: frame 0 of a group is a snapshot, the rest are merge patches.
	fn apply(&mut self, payload: &[u8]) -> Result<()> {
		match self.frames_read {
			0 => self.decoder.snapshot(payload)?,
			_ => self.decoder.delta(payload)?,
		}
		self.frames_read += 1;
		Ok(())
	}
}

/// The consumer's own subscription, resolved by the first read.
enum Track {
	Subscribing(moq_net::track::Subscribing),
	Ordered(moq_net::track::Ordered),
}

impl Track {
	/// Resolve the subscription once it is ready; an error stays for the next read to report.
	fn resolve(&mut self, waiter: &kio::Waiter) {
		if let Self::Subscribing(pending) = self
			&& let Poll::Ready(Ok(subscriber)) = pending.poll_ok(waiter)
		{
			*self = Self::Ordered(subscriber.ordered());
		}
	}

	fn poll_next_group(&mut self, waiter: &kio::Waiter) -> Poll<moq_net::Result<Option<moq_net::group::Consumer>>> {
		if let Self::Subscribing(pending) = self {
			*self = Self::Ordered(std::task::ready!(pending.poll_ok(waiter))?.ordered());
		}
		match self {
			Self::Ordered(track) => track.poll_next_group(waiter),
			Self::Subscribing(_) => unreachable!("resolved above"),
		}
	}
}

#[cfg(test)]
mod timestamps {
	use super::*;

	#[test]
	fn exact_timestamps_survive_reads() {
		for compress in [false, true] {
			let scale = moq_net::Timescale::new(90_000).unwrap();
			let info = moq_net::track::Info::default()
				.with_timescale(scale)
				.with_compress(compress);
			let track = moq_net::broadcast::Info::new()
				.produce()
				.create_track("data", info)
				.unwrap();
			let mut consumer = Consumer::<serde_json::Value>::new(track.consume(), Default::default());
			let mut newest = Consumer::<serde_json::Value>::new(track.consume(), Default::default());
			let mut producer = crate::snapshot::Producer::new(track, crate::snapshot::Config::default());
			let times = [0, 810_001, 990_007];
			for (n, ticks) in times.into_iter().enumerate() {
				let at = moq_net::Timestamp::new(ticks, scale).unwrap();
				producer
					.update(moq_net::Timed {
						value: &serde_json::json!({"n": n, "padding": "large-repeated-value-for-deltas"}),
						at: Some(at),
					})
					.unwrap();
				if n == 1 {
					producer.cut().unwrap();
				}
				let Poll::Ready(Ok(Some(value))) = consumer.poll_next(&kio::Waiter::noop()) else {
					panic!("missing state {n}")
				};
				assert_eq!(value.value["n"].as_u64().unwrap(), n as u64);
				let at = value.at.unwrap();
				assert_eq!(at.value(), ticks);
				assert_eq!(at.scale(), scale);
			}
			producer.finish().unwrap();
			assert!(matches!(
				consumer.poll_next(&kio::Waiter::noop()),
				Poll::Ready(Ok(None))
			));
			let Poll::Ready(Ok(Some(value))) = newest.poll_latest(&kio::Waiter::noop()) else {
				panic!("missing latest state")
			};
			assert_eq!(value.value["n"].as_u64().unwrap(), 2);
			assert_eq!(value.at.unwrap().value(), 990_007);
			assert_eq!(value.at.unwrap().scale(), scale);
		}
	}

	#[test]
	fn latest_advances_the_same_cursor_as_next() {
		let track = moq_net::broadcast::Info::new()
			.produce()
			.create_track("data", moq_net::track::Info::default().with_compress(true))
			.unwrap();
		let mut consumer = Consumer::<serde_json::Value>::new(track.consume(), Default::default());
		let mut producer = crate::snapshot::Producer::new(track, crate::snapshot::Config::default());
		let waiter = kio::Waiter::noop();
		for n in 0..3 {
			producer
				.update(&serde_json::json!({"n": n, "padding": "large-repeated-value-for-deltas"}))
				.unwrap();
			if n == 0 {
				let Poll::Ready(Ok(Some(value))) = consumer.poll_next(&waiter) else {
					panic!("missing first state")
				};
				assert_eq!(value.value["n"].as_u64().unwrap(), 0);
				assert_eq!(value.at, None);
			}
		}
		let Poll::Ready(Ok(Some(value))) = consumer.poll_latest(&waiter) else {
			panic!("missing latest state")
		};
		assert_eq!(value.value["n"].as_u64().unwrap(), 2);
		assert_eq!(value.at, None);
		assert!(consumer.poll_next(&waiter).is_pending());
		let n = 3;
		producer
			.update(&serde_json::json!({"n": n, "padding": "large-repeated-value-for-deltas"}))
			.unwrap();
		let Poll::Ready(Ok(Some(value))) = consumer.poll_next(&waiter) else {
			panic!("missing new state")
		};
		assert_eq!(value.value["n"].as_u64().unwrap(), 3);
		producer.finish().unwrap();
		assert!(matches!(consumer.poll_latest(&waiter), Poll::Ready(Ok(None))));
	}

	#[test]
	fn latest_ignores_datagrams() {
		let track = moq_net::broadcast::Info::new()
			.produce()
			.create_track("data", moq_net::track::Info::default())
			.unwrap();
		let mut raw = track.clone();
		let mut consumer = Consumer::<serde_json::Value>::new(track.consume(), Default::default());
		let mut producer = crate::snapshot::Producer::new(track, crate::snapshot::Config::default());
		let waiter = kio::Waiter::noop();
		for n in 0..2 {
			producer.update(&serde_json::json!({ "n": n })).unwrap();
			producer.cut().unwrap();
		}
		// A datagram shares the sequence space, so it outnumbers every group.
		raw.append_datagram(None, &b"noise"[..]).unwrap();
		let Poll::Ready(Ok(Some(value))) = consumer.poll_latest(&waiter) else {
			panic!("a datagram hid the latest state")
		};
		assert_eq!(value.value["n"], 1);
	}
}
