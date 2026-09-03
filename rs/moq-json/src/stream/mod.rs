//! Append-log JSON publishing over [`moq-net`](moq_net) tracks.
//!
//! The counterpart to [`snapshot`](crate::snapshot) mode: instead of one JSON value updated over
//! time, a stream is an ordered log of self-contained records. Every [`Producer::append`] writes one
//! JSON object as one frame, and a [`Consumer`] yields every record in order.
//!
//! The whole log rides a **single group** that is never rolled: with
//! [`ProducerConfig::compression`] on, that one group is one DEFLATE window, so every record
//! compresses against all the earlier ones. There is deliberately no group rolling (and so no
//! catch-up machinery): the only reason to roll would be moq-net's per-group frame cap, which
//! isn't worth working around here. A caller that wants to bound the record rate throttles at
//! the source (e.g. the timeline's segment cadence); a consumer that finds a gap can fetch or
//! extrapolate.
//!
//! A record that cannot be encoded or written therefore ends the track rather than continuing in a
//! second group: a log missing a record is not lossless, and a gap dressed up as a complete log is
//! worse than a visible failure. A publisher with more to say opens a new track.
//!
//! That single group is what bounds the log's history. moq-net caps a group's cached bytes, and a
//! consumer always starts at frame 0, so once the log outgrows that budget and the earliest frames
//! are evicted a new consumer fails with [`moq_net::Error::Lagged`] rather than reading a partial
//! log. (With compression the retained suffix would be undecodable anyway, since its DEFLATE window
//! depends on the evicted prefix.) The live stream is therefore bounded history by design; deep
//! history is served from a recording.
//!
//! # Choosing a layer
//!
//! [`Producer`] and [`Consumer`] own a track. [`Encoder`] and [`Decoder`] are the same logic
//! without it, for when something else is already in charge of the track; they carry the shared
//! DEFLATE window and nothing else, since a log has no group boundaries to report.

mod consumer;
mod decoder;
mod encoder;
mod producer;

pub use consumer::Consumer;
pub use decoder::{ConsumerConfig, Decoder};
pub use encoder::{Encoder, Pending, ProducerConfig};
pub use producer::Producer;

#[cfg(test)]
mod test {
	use std::task::Poll;

	use serde_json::{Value, json};

	use super::*;

	fn producer(config: ProducerConfig) -> (Producer<Value>, moq_net::track::Subscriber) {
		let track = moq_net::broadcast::Info::new()
			.produce()
			.create_track("test", None)
			.unwrap();
		let consumer = track.subscribe(None);
		(Producer::new(track, config), consumer)
	}

	fn compressed() -> ProducerConfig {
		ProducerConfig::default().with_compression(true)
	}

	fn consumer(track: moq_net::track::Subscriber, compression: bool) -> Consumer<Value> {
		Consumer::new(track, ConsumerConfig::default().with_compression(compression))
	}

	/// Drain every record currently available without blocking.
	fn drain(mut consumer: Consumer<Value>) -> Vec<Value> {
		let waiter = kio::Waiter::noop();
		let mut out = Vec::new();
		while let Poll::Ready(Ok(Some(value))) = consumer.poll_next(&waiter) {
			out.push(value);
		}
		out
	}

	#[test]
	fn plaintext_roundtrip_in_order() {
		let (mut producer, track) = producer(ProducerConfig::default());
		for n in 0..5 {
			producer.append(&json!({ "n": n })).unwrap();
		}
		producer.finish().unwrap();

		let records = drain(consumer(track, false));
		assert_eq!(records, (0..5).map(|n| json!({ "n": n })).collect::<Vec<_>>());
	}

	#[test]
	fn compressed_roundtrip_in_order() {
		let (mut producer, track) = producer(compressed());
		for n in 0..20 {
			producer.append(&json!({ "group": n, "pts": n * 2_000 })).unwrap();
		}
		producer.finish().unwrap();

		let records = drain(consumer(track, true));
		assert_eq!(records.len(), 20);
		assert_eq!(records[7], json!({ "group": 7, "pts": 14_000 }));
	}

	#[test]
	fn all_records_ride_one_group() {
		let (mut producer, track) = producer(compressed());
		for n in 0..50 {
			producer.append(&json!({ "n": n })).unwrap();
		}
		producer.finish().unwrap();

		// Never rolled: a single group holds the whole log.
		assert_eq!(track.latest(), Some(0));
		assert_eq!(drain(consumer(track, true)).len(), 50);
	}

	#[test]
	fn live_consumer_sees_each_record() {
		let (mut producer, track) = producer(compressed());
		let mut consumer = consumer(track, true);
		let waiter = kio::Waiter::noop();

		for n in 0..3 {
			producer.append(&json!({ "n": n })).unwrap();
			match consumer.poll_next(&waiter) {
				Poll::Ready(Ok(Some(value))) => assert_eq!(value, json!({ "n": n })),
				other => panic!("expected record, got {other:?}"),
			}
		}
		assert!(matches!(consumer.poll_next(&waiter), Poll::Pending));
		producer.finish().unwrap();
	}

	#[test]
	fn shared_window_shrinks_repetitive_records() {
		let (mut producer, track) = producer(compressed());
		for n in 0..8 {
			producer.append(&json!({ "group": n, "pts": n * 2_000 })).unwrap();
		}
		producer.finish().unwrap();

		let waiter = kio::Waiter::noop();
		let mut track = track.ordered();
		let Poll::Ready(Ok(Some(mut group))) = track.poll_next_group(&waiter) else {
			panic!("expected a group");
		};
		let mut sizes = Vec::new();
		while let Poll::Ready(Ok(Some(frame))) = group.poll_read_frame(&waiter) {
			sizes.push(frame.payload.len());
		}
		assert_eq!(sizes.len(), 8);
		let raw = serde_json::to_vec(&json!({ "group": 7, "pts": 14_000 })).unwrap().len();
		assert!(
			*sizes.last().unwrap() < raw / 2,
			"windowed record {} should be far below its raw size {raw}",
			sizes.last().unwrap()
		);
	}

	/// A record the encoder rejects must not have published a group first: a live consumer would
	/// advance into it and wait there even though nothing was ever appended. It still ends the
	/// track, since the log is missing the record either way.
	#[test]
	fn a_rejected_record_does_not_open_a_group() {
		// A map with non-string keys can't be represented as JSON, so serialization fails.
		let track = moq_net::broadcast::Info::new()
			.produce()
			.create_track("test", None)
			.unwrap();
		let mut subscriber = track.subscribe(None);
		let mut producer = Producer::<std::collections::BTreeMap<(u8, u8), u8>>::new(track, ProducerConfig::default());

		let mut bad = std::collections::BTreeMap::new();
		bad.insert((1, 2), 3);
		assert!(producer.append(&bad).is_err());

		assert_eq!(subscriber.latest(), None, "a rejected record opened a group");

		let waiter = kio::Waiter::noop();
		assert!(
			matches!(subscriber.poll_recv_group(&waiter), Poll::Ready(Err(_))),
			"the log is missing a record, so the track must end rather than stay writable"
		);
	}

	/// A track whose timescale is extreme enough that converting a wall-clock timestamp into it
	/// overflows, so `write_frame` rejects every frame. That stands in for any post-`append_group`
	/// write failure (the reported one is a frame over moq-net's 32 MB per-group cache) without
	/// allocating 32 MB to provoke it.
	fn rejecting_track() -> moq_net::track::Producer {
		let mut info = moq_net::track::Info::default();
		info.timescale = moq_net::Timescale::new((1u64 << 62) - 1).unwrap();

		moq_net::broadcast::Info::new()
			.produce()
			.create_track("test", Some(info))
			.unwrap()
	}

	/// A failed write must reach the consumer, not just the caller. A clean close drains a reader to
	/// `None`, which is exactly what a completed log looks like, so a truncated log would be
	/// indistinguishable from a whole one.
	#[test]
	fn a_failed_write_aborts_the_track() {
		let track = rejecting_track();
		let mut subscriber = track.subscribe(None);
		let mut producer = Producer::<Value>::new(track, ProducerConfig::default());

		assert!(matches!(producer.append(&json!({ "n": 1 })), Err(crate::Error::Net(_))));

		let waiter = kio::Waiter::noop();
		assert!(
			matches!(subscriber.poll_recv_group(&waiter), Poll::Ready(Err(_))),
			"a truncated log must surface an error rather than read as a completed one"
		);
	}

	/// The track ends with the group, so nothing opens a second one and splits the log. The retry
	/// reports the ended track rather than the [`Error::Desync`](crate::Error::Desync) the dropped
	/// record left on the encoder, which says nothing about why the log stopped.
	#[test]
	fn a_failed_write_ends_the_track() {
		let track = rejecting_track();
		let mut producer = Producer::<Value>::new(track, ProducerConfig::default().with_compression(true));

		assert!(matches!(producer.append(&json!({ "n": 1 })), Err(crate::Error::Net(_))));

		// The retry reports the abort rather than the `Error::Desync` the dropped record left on the
		// encoder, which says nothing about why the log stopped.
		assert!(
			matches!(producer.append(&json!({ "n": 2 })), Err(crate::Error::Net(_))),
			"a second append must fail on the ended track rather than open another group"
		);

		// A subscriber taken after the abort still exists; it surfaces the failure on its first read,
		// which is how a late reader learns the log is truncated.
		let waiter = kio::Waiter::noop();
		assert!(matches!(
			producer.consume().poll_recv_group(&waiter),
			Poll::Ready(Err(_))
		));
	}

	/// A completed log is still readable, so finishing must not end the track the way an abort does.
	/// The append that follows fails on the closed track without turning it into a failure.
	#[test]
	fn appending_after_finish_fails_without_aborting() {
		let (mut producer, _track) = producer(compressed());
		producer.append(&json!({ "n": 0 })).unwrap();
		producer.finish().unwrap();

		assert!(producer.append(&json!({ "n": 1 })).is_err());
		assert_eq!(drain(consumer(producer.consume(), true)), vec![json!({ "n": 0 })]);
	}

	/// A stream is one group. A publisher that opens a second lost whatever would have completed the
	/// first, so the read reports that rather than handing back the remainder as a continuous log.
	/// A boundary-only check would never look at the track again while the first group is open, so
	/// this parks forever without the eager check. Written by hand because this producer never rolls.
	#[test]
	fn a_second_group_is_reported_while_the_first_is_open() {
		let mut track = moq_net::broadcast::Info::new()
			.produce()
			.create_track("test", None)
			.unwrap();

		// Ask for a replay window, so the first group is delivered rather than skipped by the
		// subscriber's default max-age budget once a newer group exists.
		let subscription = moq_net::track::Subscription::default().with_max_age(std::time::Duration::from_secs(30));
		let subscriber = track.subscribe(subscription);

		// Both groups stay open, the way a publisher writing to two at once leaves them.
		let mut first = track.append_group().unwrap();
		first
			.write_frame(moq_net::Timestamp::now(), br#"{"n":0}"#.as_slice())
			.unwrap();
		let mut second = track.append_group().unwrap();
		second
			.write_frame(moq_net::Timestamp::now(), br#"{"n":1}"#.as_slice())
			.unwrap();

		let mut consumer = consumer(subscriber, false);
		let waiter = kio::Waiter::noop();

		assert!(matches!(
			consumer.poll_next(&waiter),
			Poll::Ready(Ok(Some(value))) if value == json!({ "n": 0 })
		));
		assert!(matches!(
			consumer.poll_next(&waiter),
			Poll::Ready(Err(crate::Error::Rolled))
		));

		// Sticky: a later read must not report the rest of the first group as a whole log.
		first
			.write_frame(moq_net::Timestamp::now(), br#"{"n":2}"#.as_slice())
			.unwrap();
		assert!(matches!(
			consumer.poll_next(&waiter),
			Poll::Ready(Err(crate::Error::Rolled))
		));
	}

	#[test]
	fn embedded_newlines_survive() {
		// Each record is its own frame (one JSON object), and JSON escapes control characters, so a
		// string value containing a newline round-trips cleanly.
		let (mut producer, track) = producer(compressed());
		let value = json!({ "s": "line1\nline2\ttab", "u": "a\u{000a}b" });
		for _ in 0..4 {
			producer.append(&value).unwrap();
		}
		producer.finish().unwrap();

		let records = drain(consumer(track, true));
		assert_eq!(records, vec![value.clone(), value.clone(), value.clone(), value]);
	}
}
