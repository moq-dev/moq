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
//! the source (e.g. the timeline's granularity); a consumer that finds a gap can fetch or
//! extrapolate.
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
pub use encoder::{Encoder, ProducerConfig};
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
		let (mut producer, mut track) = producer(compressed());
		for n in 0..8 {
			producer.append(&json!({ "group": n, "pts": n * 2_000 })).unwrap();
		}
		producer.finish().unwrap();

		let waiter = kio::Waiter::noop();
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
