//! Lossless append-log binary publishing over [`moq-net`](moq_net) tracks.
//!
//! An ordered log of opaque payloads, for consumers that care about every one (an event log, a
//! sequence of samples). Nothing is ever superseded: a consumer yields each payload in the order it
//! was appended. For a latest-value document, use [`snapshot`](crate::snapshot) instead.
//!
//! On the wire the log is a single group that is never rolled, one payload per frame. A payload
//! that cannot be written ends the track rather than opening a second group: a log missing a record
//! is not lossless, and a gap dressed up as a complete log is worse than a visible failure. With
//! [`ProducerConfig::compression`] on, that one group is one sync-flushed DEFLATE stream, so each
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

	/// A conforming publisher writes one group, but the consumer tolerates one that writes more:
	/// each group starts a cold window and its payloads are still delivered. Written by hand
	/// because this producer never rolls.
	#[test]
	fn a_second_group_is_still_read() {
		let mut track = moq_net::broadcast::Info::new()
			.produce()
			.create_track("test", None)
			.unwrap();
		let subscriber = track.subscribe(None);

		let expected = payloads(4);
		for pair in expected.chunks(2) {
			// Each group is its own DEFLATE stream, which is what the recovery roll produces.
			let mut flate = moq_flate::Encoder::new();
			let mut group = track.append_group().unwrap();
			for payload in pair {
				group
					.write_frame(moq_net::Timestamp::now(), flate.frame(payload))
					.unwrap();
			}
			group.finish().unwrap();
		}
		track.finish().unwrap();

		assert_eq!(drain(subscriber, true), expected);
	}

	/// Groups are separate QUIC streams, so a second group can land before the first. Reading in
	/// arrival order still delivers both; the monotonic `next_group` would skip past the lower
	/// sequence and drop it for good.
	#[test]
	fn a_late_group_is_not_dropped() {
		let mut track = moq_net::broadcast::Info::new()
			.produce()
			.create_track("test", None)
			.unwrap();
		let subscriber = track.subscribe(None);

		// Publish sequence 1 before sequence 0, the way reordering delivers them.
		for sequence in [1u64, 0] {
			let mut flate = moq_flate::Encoder::new();
			let mut group = track.create_group(moq_net::group::Info { sequence }).unwrap();
			group
				.write_frame(moq_net::Timestamp::now(), flate.frame(&[sequence as u8; 8]))
				.unwrap();
			group.finish().unwrap();
		}
		track.finish().unwrap();

		// Delivered in arrival order rather than sequence order, but nothing is lost.
		assert_eq!(
			drain(subscriber, true),
			vec![Bytes::from(vec![1u8; 8]), Bytes::from(vec![0u8; 8])]
		);
	}

	/// `finish` closes the underlying track, so a later append fails rather than being silently
	/// accepted, and that holds for every clone since they share one track.
	#[test]
	fn appending_after_finish_fails_on_every_clone() {
		let (mut producer, _track) = producer(false);
		let mut clone = producer.clone();

		producer.append(&b"first"[..]).unwrap();
		producer.finish().unwrap();

		assert!(producer.append(&b"late"[..]).is_err());
		assert!(clone.append(&b"late"[..]).is_err());
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
