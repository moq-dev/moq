//! Append-log JSON publishing over [`moq-net`](moq_net) tracks.
//!
//! The counterpart to [`snapshot`](crate::snapshot) mode: instead of one JSON value updated over
//! time, a stream is an ordered log of self-contained records. Every [`Producer::append`] writes one
//! JSON object as one frame, and a [`Consumer`] yields every record in order.
//!
//! The whole log rides a **single group** that is never rolled: on a
//! [`compress`](moq_net::track::Info::compress)ed track, that one group is one DEFLATE window, so
//! every record compresses against all the earlier ones. There is deliberately no group rolling (and so no
//! catch-up machinery): a moq-net group has no size cap. A caller that wants to bound the record rate throttles at
//! the source (e.g. the timeline's segment cadence); a consumer that finds a gap can fetch or
//! extrapolate.
//!
//! A record that cannot be encoded or written therefore ends the track rather than continuing in a
//! second group: a log missing a record is not lossless, and a gap dressed up as a complete log is
//! worse than a visible failure. A publisher with more to say opens a new track.
//!
//! # Budget
//!
//! A log accepts appends for its track's life; only a record larger than one frame may be
//! ([`MAX_CACHE_BYTES`](moq_net::group::MAX_CACHE_BYTES), 32 MiB) is refused, with
//! [`moq_net::Error::FrameTooLarge`], before it is written. moq-net holds only the group's newest
//! 32 MiB and sheds older records from its front, so a consumer that falls further behind, or
//! joins after the front is shed, gets [`moq_net::Error::Evicted`].
//!
//! # Choosing a layer
//!
//! [`Producer`] and [`Consumer`] own a track. [`Encoder`] and [`Decoder`] are the same logic
//! without it, for when something else is already in charge of the track. A log has no group
//! boundaries to report, so they carry no state.

pub mod consumer;
mod decoder;
mod encoder;
pub mod producer;

pub use consumer::Consumer;
pub use decoder::Decoder;
pub use encoder::{Config, Encoder, Pending};
pub use producer::Producer;

#[cfg(test)]
use futures::FutureExt;

#[cfg(test)]
mod test {
	use std::task::Poll;

	use serde_json::{Value, json};

	use super::*;

	/// Refused before the log's group opens, so the log carries on rather than ending.
	#[test]
	fn a_timed_track_refuses_a_bare_publish() {
		let (mut producer, track) = producer_on(Config::default(), timed());
		assert!(matches!(
			producer.append(&json!({ "n": 0 })),
			Err(crate::Error::Net(moq_net::Error::TimestampMismatch))
		));
		let at = moq_net::Timestamp::from_millis(100).unwrap();
		producer
			.append(moq_net::Timed::from(&json!({ "n": 1 })).at(at))
			.unwrap();
		assert!(matches!(
			producer.append(&json!({ "n": 2 })),
			Err(crate::Error::Net(moq_net::Error::TimestampMismatch))
		));
		producer
			.append(moq_net::Timed::from(&json!({ "n": 3 })).at(at))
			.unwrap();
		producer.finish().unwrap();
		assert_eq!(drain(consume(track)), vec![json!({ "n": 1 }), json!({ "n": 3 })]);
	}

	#[test]
	fn a_bare_publish_stays_untimed() {
		let track = moq_net::broadcast::Info::new()
			.produce()
			.create_track("untimed", moq_net::track::Info::default())
			.unwrap();
		let mut groups = track.subscribe(None);
		let mut producer = Producer::new(track, Config::default());
		producer.append(&json!({ "n": 1 })).unwrap();
		let waiter = kio::Waiter::noop();
		let Poll::Ready(Ok(Some(mut group))) = groups.poll_recv_group(&waiter) else {
			panic!("expected an untimed group");
		};
		let Poll::Ready(Ok(Some(frame))) = group.poll_read_frame(&waiter) else {
			panic!("expected an untimed frame");
		};
		assert_eq!(frame.timestamp, None);
	}

	/// A millisecond track: the frames written here carry timestamps.
	fn timed() -> moq_net::track::Info {
		moq_net::track::Info::default().with_timescale(moq_net::Timescale::MILLI)
	}

	fn producer(config: Config) -> (Producer<Value>, moq_net::track::Subscriber) {
		producer_on(config, moq_net::track::Info::default())
	}

	fn producer_on(config: Config, info: moq_net::track::Info) -> (Producer<Value>, moq_net::track::Subscriber) {
		let track = moq_net::broadcast::Info::new()
			.produce()
			.create_track("test", info)
			.unwrap();
		let consumer = track.subscribe(None);
		(Producer::new(track, config), consumer)
	}

	/// Demand follows the track's subscribers, so a producer can idle while nobody is watching.
	#[test]
	fn demand_follows_subscribers() {
		let (producer, consumer) = producer(Config::default());
		let demand = producer.demand();
		assert_eq!(demand.wanted(), Some(0));

		drop(consumer);
		assert_eq!(demand.wanted(), None);

		let _consumer = producer.consume();
		assert_eq!(demand.wanted(), Some(0));
	}

	/// A compressed track.
	fn compressed() -> moq_net::track::Info {
		moq_net::track::Info::default().with_compress(true)
	}

	fn consume(track: moq_net::track::Subscriber) -> Consumer<Value> {
		Consumer::new(track, Config::default())
	}

	/// Drain every record currently available without blocking.
	fn drain(mut consumer: Consumer<Value>) -> Vec<Value> {
		let waiter = kio::Waiter::noop();
		let mut out = Vec::new();
		while let Poll::Ready(Ok(Some(value))) = consumer.poll_next(&waiter) {
			out.push(value.value);
		}
		out
	}

	#[test]
	fn plaintext_roundtrip_in_order() {
		let (mut producer, track) = producer(Config::default());
		for n in 0..5 {
			producer.append(&json!({ "n": n })).unwrap();
		}
		producer.finish().unwrap();

		let records = drain(consume(track));
		assert_eq!(records, (0..5).map(|n| json!({ "n": n })).collect::<Vec<_>>());
	}

	/// Each record keeps its own capture time, and the returned size is the encoded frame.
	#[test]
	fn a_stamped_append_writes_its_capture_time() {
		let (mut producer, _track) = producer_on(Config::default(), timed().with_compress(true));
		let mut groups = producer.consume();
		let captured = moq_net::Timestamp::from_millis(1_234).unwrap();
		let record = json!({ "n": 1 });
		let size = producer.append(moq_net::Timed::from(&record).at(captured)).unwrap();

		let waiter = kio::Waiter::noop();
		let Poll::Ready(Ok(Some(mut group))) = groups.poll_recv_group(&waiter) else {
			panic!("expected a group");
		};
		let Poll::Ready(Ok(Some(frame))) = group.poll_read_frame(&waiter) else {
			panic!("expected a frame");
		};
		assert_eq!(frame.timestamp.unwrap().as_micros(), captured.as_micros());
		assert_eq!(size, frame.payload.len());
	}

	#[test]
	fn all_records_ride_one_group() {
		let (mut producer, mut track) = producer_on(Config::default(), compressed());
		for n in 0..50 {
			producer.append(&json!({ "n": n })).unwrap();
		}
		producer.finish().unwrap();

		// Never rolled: a single group holds the whole log.
		assert_eq!(
			track.largest().now_or_never().flatten().map(|position| position.group),
			Some(0)
		);
		assert_eq!(drain(consume(track)).len(), 50);
	}

	#[test]
	fn live_consumer_sees_each_record() {
		let (mut producer, track) = producer_on(Config::default(), compressed());
		let mut consumer = consume(track);
		let waiter = kio::Waiter::noop();

		for n in 0..3 {
			producer.append(&json!({ "n": n })).unwrap();
			match consumer.poll_next(&waiter) {
				Poll::Ready(Ok(Some(value))) => assert_eq!(value.value, json!({ "n": n })),
				other => panic!("expected record, got {other:?}"),
			}
		}
		assert!(matches!(consumer.poll_next(&waiter), Poll::Pending));
		producer.finish().unwrap();
	}

	/// A record the encoder rejects must not have published a group first: a live consumer would
	/// advance into it and wait there even though nothing was ever appended. It still ends the
	/// track, since the log is missing the record either way.
	#[test]
	fn a_rejected_record_does_not_open_a_group() {
		// A map with non-string keys can't be represented as JSON, so serialization fails.
		let track = moq_net::broadcast::Info::new()
			.produce()
			.create_track("test", timed())
			.unwrap();
		let mut subscriber = track.subscribe(None);
		let mut producer = Producer::<std::collections::BTreeMap<(u8, u8), u8>>::new(track, Config::default());

		let mut bad = std::collections::BTreeMap::new();
		bad.insert((1, 2), 3);
		assert!(
			producer
				.append(moq_net::Timed::from(&bad).at(moq_net::Timestamp::from_millis(0).unwrap()))
				.is_err()
		);

		assert_eq!(
			subscriber
				.largest()
				.now_or_never()
				.flatten()
				.map(|position| position.group),
			None,
			"a rejected record opened a group"
		);

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
		let mut info = timed();
		info.timescale = Some(moq_net::Timescale::new((1u64 << 62) - 1).unwrap());

		moq_net::broadcast::Info::new()
			.produce()
			.create_track("test", Some(info))
			.unwrap()
	}

	/// `value` at a time [`rejecting_track`] can't hold: it matches the timed track, so the
	/// append gets as far as the group before the conversion fails.
	fn late(value: &Value) -> moq_net::Timed<&Value> {
		moq_net::Timed::from(value).at(moq_net::Timestamp::from_millis(10_000).unwrap())
	}

	/// A failed write must reach the consumer, not just the caller. A clean close drains a reader to
	/// `None`, which is exactly what a completed log looks like, so a truncated log would be
	/// indistinguishable from a whole one.
	#[test]
	fn a_failed_write_aborts_the_track() {
		let track = rejecting_track();
		let mut subscriber = track.subscribe(None);
		let mut producer = Producer::<Value>::new(track, Config::default());

		assert!(matches!(
			producer.append(late(&json!({ "n": 1 }))),
			Err(crate::Error::Net(_))
		));

		let waiter = kio::Waiter::noop();
		assert!(
			matches!(subscriber.poll_recv_group(&waiter), Poll::Ready(Err(_))),
			"a truncated log must surface an error rather than read as a completed one"
		);
	}

	/// The track ends with the group, so nothing opens a second one and splits the log.
	#[test]
	fn a_failed_write_ends_the_track() {
		let track = rejecting_track();
		let mut producer = Producer::<Value>::new(track, Config::default());

		assert!(matches!(
			producer.append(late(&json!({ "n": 1 }))),
			Err(crate::Error::Net(_))
		));

		assert!(
			matches!(producer.append(late(&json!({ "n": 2 }))), Err(crate::Error::Net(_))),
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
		let (mut producer, _track) = producer_on(Config::default(), compressed());
		producer.append(&json!({ "n": 0 })).unwrap();
		producer.finish().unwrap();

		assert!(producer.append(&json!({ "n": 1 })).is_err());
		assert_eq!(drain(consume(producer.consume())), vec![json!({ "n": 0 })]);
	}

	/// A stream is one group. A publisher that opens a second lost whatever would have completed the
	/// first, so the read reports that rather than handing back the remainder as a continuous log.
	/// A boundary-only check would never look at the track again while the first group is open, so
	/// this parks forever without the eager check. Written by hand because this producer never rolls.
	#[test]
	fn a_second_group_is_reported_while_the_first_is_open() {
		let track = moq_net::broadcast::Info::new()
			.produce()
			.create_track("test", timed())
			.unwrap();

		// Ask for a replay window, so the first group is delivered rather than skipped by the
		// subscriber's default max delay budget once a newer group exists.
		let subscription = moq_net::track::Subscription::default().with_max_delay(std::time::Duration::from_secs(30));
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

		let mut consumer = consume(subscriber);
		let waiter = kio::Waiter::noop();

		assert!(matches!(
			consumer.poll_next(&waiter),
			Poll::Ready(Ok(Some(value))) if value.value == json!({ "n": 0 })
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

	fn info(compress: bool) -> moq_net::track::Info {
		moq_net::track::Info::default().with_compress(compress)
	}

	fn refused(result: crate::Result<usize>) -> bool {
		matches!(result, Err(crate::Error::Net(moq_net::Error::FrameTooLarge)))
	}

	/// A record larger than a frame is refused before anything is written, so the log carries on:
	/// the next record lands and decodes.
	#[test]
	fn an_oversized_record_is_refused_and_the_log_continues() {
		for compression in [false, true] {
			let (mut producer, track) = producer_on(Config::default(), info(compression));
			producer.append(&json!({ "n": 0 })).unwrap();

			let oversized = Value::String("x".repeat(moq_net::group::MAX_CACHE_BYTES as usize));
			assert!(refused(producer.append(&oversized)), "compression={compression}");

			producer.append(&json!({ "n": 1 })).unwrap();
			producer.finish().unwrap();

			assert_eq!(
				drain(consume(track)),
				vec![json!({ "n": 0 }), json!({ "n": 1 })],
				"compression={compression}"
			);
		}
	}

	/// A log has no budget: it carries more records than a group once could, and reads back whole.
	#[test]
	fn a_log_accepts_every_append() {
		for compression in [false, true] {
			let (mut producer, track) = producer_on(Config::default(), info(compression));
			for n in 0..10_000 {
				producer.append(&json!({ "n": n })).unwrap();
			}
			producer.finish().unwrap();

			let records = drain(consume(track));
			assert_eq!(records.len(), 10_000, "compression={compression}");
			assert_eq!(records.last(), Some(&json!({ "n": 9_999 })));
		}
	}

	#[test]
	fn embedded_newlines_survive() {
		// Each record is its own frame (one JSON object), and JSON escapes control characters, so a
		// string value containing a newline round-trips cleanly.
		let (mut producer, track) = producer_on(Config::default(), compressed());
		let value = json!({ "s": "line1\nline2\ttab", "u": "a\u{000a}b" });
		for _ in 0..4 {
			producer.append(&value).unwrap();
		}
		producer.finish().unwrap();

		let records = drain(consume(track));
		assert_eq!(records, vec![value.clone(), value.clone(), value.clone(), value]);
	}
}
