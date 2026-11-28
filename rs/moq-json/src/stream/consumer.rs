//! Consuming an ordered log from a track: a [`Decoder`] plus the track it reads from.

use std::task::{Poll, ready};

use serde::de::DeserializeOwned;

use super::Decoder;

pub use super::Config;
use crate::Result;

/// Consumes an ordered log of JSON records from a track, yielding every record in order.
///
/// A [`Decoder`] that owns its track: it reads one record per frame, in order. The log is a single
/// group, which is what makes the mode lossless: rolling to a second group means the records that
/// would have completed the first are gone, so a [`Producer`](super::Producer) that cannot write
/// ends the track instead. A second group is therefore a broken publisher, and reading it would
/// present a gap as a continuous log, so it fails with [`Error::Rolled`](crate::Error::Rolled)
/// rather than yielding the remainder. When something else already owns the track, use the
/// [`Decoder`] directly.
///
/// The failure does not wait for the first group to end: whatever has already arrived in it is
/// yielded, and the read then fails rather than blocking on a group a broken publisher may never
/// finish.
pub struct Consumer<T> {
	track: moq_net::track::Subscriber,
	log: Log,
	decoder: Decoder<T>,
}

/// Where a [`Consumer`] is in the log's one group.
enum Log {
	/// Waiting for the group.
	Waiting,
	/// Reading the group.
	Reading(moq_net::group::Consumer),
	/// The group is exhausted, so only the track's end remains, and any group is a second one.
	Drained,
	/// A second group was seen. Sticky: the records it displaced are gone, so every later read
	/// fails too rather than reporting the rest of the log as a whole one.
	Rolled,
}

impl<T: DeserializeOwned> Consumer<T> {
	/// Create a consumer reading from the given track subscriber.
	///
	/// Takes a subscriber where [`snapshot::Consumer::new`](crate::snapshot::Consumer::new) takes the
	/// track: every record matters here, so the caller picks the subscription's delay budget and
	/// priority, while a snapshot only wants the newest value and subscribes itself at a zero delay.
	/// A [compressed](moq_net::track::Info::compression) track reads the same: moq-net inflates it.
	/// The log is read oldest first whatever order the subscription asked, so a second group a
	/// broken publisher opens is caught as `Rolled` rather than read ahead of the first.
	pub fn new(mut track: moq_net::track::Subscriber, config: Config) -> Self {
		let mut subscription = track.subscription();
		for range in &mut subscription.ranges {
			range.order = moq_net::track::Order::Ascending;
		}
		// A track that already ended holds no group to read out of order.
		let _ = track.update(subscription);
		Self {
			track,
			log: Log::Waiting,
			decoder: Decoder::new(config),
		}
	}

	/// Get the next record and its exact frame timestamp, or `None` once the track ends.
	#[cfg_attr(feature = "async", kio::wrapper)]
	pub fn poll_next(&mut self, waiter: &kio::Waiter) -> Poll<Result<Option<crate::Timed<T>>>> {
		loop {
			let group = match &mut self.log {
				Log::Rolled => return Poll::Ready(Err(crate::Error::Rolled)),
				Log::Reading(group) => group,
				// Arrival order rather than sequence order, because there is only ever one group to
				// take and a second one has to be seen whatever its sequence. The monotonic
				// `poll_next_group` would drop a late lower sequence, which is the very loss this
				// has to report.
				Log::Waiting | Log::Drained => {
					self.log = match (ready!(self.track.poll_recv_group(waiter)?), &self.log) {
						(None, _) => return Poll::Ready(Ok(None)),
						(Some(group), Log::Waiting) => Log::Reading(group),
						(Some(_), _) => Log::Rolled,
					};
					continue;
				}
			};

			match group.poll_read_frame(waiter)? {
				Poll::Ready(Some(frame)) => {
					return Poll::Ready(Ok(Some(crate::Timed {
						value: self.decoder.decode(&frame.payload)?,
						at: frame.timestamp,
					})));
				}
				// The log's one group is exhausted. Keep polling the track so a clean end still
				// reports the log as complete, and so a second group is caught as `Rolled`.
				Poll::Ready(None) => self.log = Log::Drained,
				// Nothing more in the group yet, so ask the track before parking on it. A publisher
				// that opens a second group and leaves the first open would otherwise hold this read
				// open forever, on a log that already lost the records the second one displaced.
				// Both polls register, so either source wakes this read.
				Poll::Pending => match self.track.poll_recv_group(waiter)? {
					Poll::Ready(Some(_)) => self.log = Log::Rolled,
					// A finished track does not truncate the group in hand; its frames may still arrive.
					Poll::Ready(None) | Poll::Pending => return Poll::Pending,
				},
			}
		}
	}
}

#[cfg(test)]
mod timestamps {
	use super::*;

	#[test]
	fn exact_timestamps_survive_reads() {
		for compression in [moq_net::track::Compression::None, moq_net::track::Compression::Deflate] {
			let scale = moq_net::Timescale::new(90_000).unwrap();
			let info = moq_net::track::Info::default()
				.with_timescale(scale)
				.with_compression(compression);
			let track = moq_net::broadcast::Info::new()
				.produce()
				.create_track("data", info)
				.unwrap();
			let mut subscriber = track.subscribe(None);
			let mut subscription = subscriber.subscription();
			subscription.ranges = vec![moq_net::track::Range::ALL];
			subscriber.update(subscription).unwrap();
			let config = crate::stream::Config::default();
			let mut producer = crate::stream::Producer::new(track, config.clone());
			let mut consumer = Consumer::<serde_json::Value>::new(subscriber, config);
			let times = [0, 810_001, 990_007];
			for (n, ticks) in times.into_iter().enumerate() {
				let at = moq_net::Timestamp::new(ticks, scale).unwrap();
				producer
					.append(
						&serde_json::json!({"n": n, "padding": "large-repeated-value-for-deltas"}),
						at,
					)
					.unwrap();
			}
			let _finished = producer.finish().unwrap();
			for (n, ticks) in times.into_iter().enumerate() {
				let Poll::Ready(Ok(Some(value))) = consumer.poll_next(&kio::Waiter::noop()) else {
					panic!("missing state {n}")
				};
				assert_eq!(value.value["n"].as_u64().unwrap(), n as u64);
				let at = value.at;
				assert_eq!(at.value(), ticks);
				assert_eq!(at.scale(), Some(scale));
			}
			assert!(matches!(
				consumer.poll_next(&kio::Waiter::noop()),
				Poll::Ready(Ok(None))
			));
		}
	}
}
