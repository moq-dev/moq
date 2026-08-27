//! Lossless append-log binary publishing over [`moq-net`](moq_net) tracks.
//!
//! An ordered log of opaque payloads, for consumers that care about every one (an event log, a
//! sequence of samples). Nothing is ever superseded: a consumer yields each payload in the order it
//! was appended. For a latest-value document, use [`snapshot`](crate::snapshot) instead.
//!
//! On the wire the log is a single group that is never rolled, one payload per frame. With
//! [`ProducerConfig::compression`] on, that group is one sync-flushed DEFLATE stream, so each
//! payload compresses against the earlier ones and a run of similar payloads shrinks sharply.

mod consumer;
mod producer;

pub use consumer::{Consumer, ConsumerConfig};
pub use producer::{Producer, ProducerConfig};

#[cfg(test)]
mod test {
	use std::task::Poll;

	use bytes::Bytes;

	use super::*;

	fn producer(compression: bool) -> (Producer, moq_net::track::Subscriber) {
		let track = moq_net::broadcast::Info::new()
			.produce()
			.create_track("test", None)
			.unwrap();
		let consumer = track.subscribe(None);
		(
			Producer::new(track, ProducerConfig::default().with_compression(compression)),
			consumer,
		)
	}

	/// Drain every payload currently available without blocking.
	fn drain(track: moq_net::track::Subscriber, compression: bool) -> Vec<Bytes> {
		let mut consumer = Consumer::new(track, ConsumerConfig::default().with_compression(compression));
		let waiter = kio::Waiter::noop();
		let mut out = Vec::new();
		while let Poll::Ready(Ok(Some(payload))) = consumer.poll_next(&waiter) {
			out.push(payload);
		}
		out
	}

	fn payloads(count: u8) -> Vec<Bytes> {
		(0..count).map(|n| Bytes::from(vec![n; 16])).collect()
	}

	#[test]
	fn every_payload_survives_in_order() {
		let (mut producer, track) = producer(false);
		let expected = payloads(5);
		for payload in &expected {
			producer.append(payload.clone()).unwrap();
		}
		producer.finish().unwrap();

		// One group holds the whole log, unlike snapshot's group-per-value.
		assert_eq!(track.latest(), Some(0));
		assert_eq!(drain(track, false), expected);
	}

	#[test]
	fn compressed_roundtrip_in_order() {
		let (mut producer, track) = producer(true);
		let expected = payloads(20);
		for payload in &expected {
			producer.append(payload.clone()).unwrap();
		}
		producer.finish().unwrap();

		assert_eq!(drain(track, true), expected);
	}

	/// The window spans the whole group, so a repeated payload costs almost nothing after the first.
	#[test]
	fn the_shared_window_shrinks_repetitive_payloads() {
		let (mut producer, mut track) = producer(true);
		let payload = Bytes::from(b"the quick brown fox".repeat(16));
		for _ in 0..8 {
			producer.append(payload.clone()).unwrap();
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
		assert!(
			*sizes.last().unwrap() < sizes[0],
			"windowed frame {} should be below the first {}",
			sizes.last().unwrap(),
			sizes[0]
		);
	}

	/// Clones share one track and one window, so two owners interleave into a single ordered log.
	#[test]
	fn clones_append_into_one_log() {
		let (mut producer, track) = producer(true);
		let mut clone = producer.clone();

		producer.append(&b"a"[..]).unwrap();
		clone.append(&b"b"[..]).unwrap();
		producer.append(&b"c"[..]).unwrap();
		producer.finish().unwrap();

		assert_eq!(
			drain(track, true),
			vec![
				Bytes::from_static(b"a"),
				Bytes::from_static(b"b"),
				Bytes::from_static(b"c")
			]
		);
	}

	#[test]
	fn a_finished_track_ends_the_consumer() {
		let (mut producer, track) = producer(false);
		producer.append(&b"only"[..]).unwrap();
		producer.finish().unwrap();

		let mut consumer = Consumer::new(track, ConsumerConfig::default());
		let waiter = kio::Waiter::noop();
		assert!(matches!(consumer.poll_next(&waiter), Poll::Ready(Ok(Some(_)))));
		assert!(matches!(consumer.poll_next(&waiter), Poll::Ready(Ok(None))));
	}
}
