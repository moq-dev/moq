//! Lossy latest-value binary publishing over [`moq-net`](moq_net) tracks.
//!
//! One opaque value updated over time, for consumers that only care about the current state (a
//! poster image, a serialized state blob). This mode is **lossy** by design: a consumer yields only
//! the most recent value. A late joiner (or a consumer that falls behind) jumps straight to the
//! newest group, and older groups are dropped entirely. For an ordered log where every payload is
//! preserved, use [`stream`](crate::stream) instead.
//!
//! On the wire each value is one group holding one frame, so a group is self-contained and a
//! consumer never needs an older one. With [`ProducerConfig::compression`] on, that frame is its own
//! raw DEFLATE stream; there is no window to share across a single-frame group.

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

	fn consumer(track: moq_net::track::Subscriber, compression: bool) -> Consumer {
		Consumer::new(track, ConsumerConfig::default().with_compression(compression))
	}

	/// Drain every value currently available without blocking.
	fn drain(mut consumer: Consumer) -> Vec<Bytes> {
		let waiter = kio::Waiter::noop();
		let mut out = Vec::new();
		while let Poll::Ready(Ok(Some(payload))) = consumer.poll_next(&waiter) {
			out.push(payload);
		}
		out
	}

	#[test]
	fn one_group_per_update() {
		let (mut producer, track) = producer(false);
		producer.update(&b"first"[..]).unwrap();
		producer.update(&b"second"[..]).unwrap();
		producer.finish().unwrap();

		// Two updates => two groups. A consumer that joins after both only sees the latest.
		assert_eq!(track.latest(), Some(1));
		assert_eq!(drain(consumer(track, false)), vec![Bytes::from_static(b"second")]);
	}

	#[test]
	fn live_consumer_sees_each_update() {
		let (mut producer, track) = producer(false);
		let mut consumer = consumer(track, false);
		let waiter = kio::Waiter::noop();

		for n in 0..3u8 {
			producer.update(vec![n]).unwrap();
			match consumer.poll_next(&waiter) {
				Poll::Ready(Ok(Some(payload))) => assert_eq!(&payload[..], &[n]),
				other => panic!("expected value, got {other:?}"),
			}
		}
	}

	#[test]
	fn compressed_roundtrip() {
		let (mut producer, track) = producer(true);
		let payload = Bytes::from(b"the quick brown fox".repeat(64));
		producer.update(payload.clone()).unwrap();
		producer.finish().unwrap();

		assert_eq!(drain(consumer(track, true)), vec![payload]);
	}

	/// The compressed frame on the wire is much smaller than the value it carries, so a consumer
	/// that ignored the flag would read garbage rather than the payload.
	#[test]
	fn compression_shrinks_the_frame() {
		let (mut producer, track) = producer(true);
		let payload = Bytes::from(b"the quick brown fox".repeat(64));
		producer.update(payload.clone()).unwrap();
		producer.finish().unwrap();

		// Read the raw frame, bypassing the consumer's decompression.
		let waiter = kio::Waiter::noop();
		let Poll::Ready(Ok(Some(mut group))) = track.ordered().poll_next_group(&waiter) else {
			panic!("expected a group");
		};
		let Poll::Ready(Ok(Some(frame))) = group.poll_read_frame(&waiter) else {
			panic!("expected a frame");
		};
		assert!(
			frame.payload.len() < payload.len() / 4,
			"compressed frame {} should be far below the raw {}",
			frame.payload.len(),
			payload.len()
		);
	}

	/// `append_group` publishes immediately, so rejecting the frame inside `write_frame` would leave
	/// an empty newest group behind. A snapshot consumer jumps to the newest, so the previous value
	/// would vanish even though the update reported an error.
	#[test]
	fn a_rejected_update_leaves_the_previous_value_readable() {
		let (mut producer, track) = producer(false);
		producer.update(&b"keep"[..]).unwrap();

		let oversized = Bytes::from(vec![0u8; moq_net::group::MAX_CACHE_BYTES as usize + 1]);
		assert!(producer.update(oversized).is_err());
		producer.finish().unwrap();

		// A reader arriving now still finds the last good value, not an empty superseding group.
		assert_eq!(drain(consumer(track, false)), vec![Bytes::from_static(b"keep")]);
	}

	/// `finish` closes the underlying track, so a later update fails rather than being silently
	/// accepted, and that holds for every clone since they share one track.
	#[test]
	fn updating_after_finish_fails_on_every_clone() {
		let (mut producer, _track) = producer(false);
		let mut clone = producer.clone();

		producer.update(&b"first"[..]).unwrap();
		producer.finish().unwrap();

		assert!(producer.update(&b"late"[..]).is_err());
		assert!(clone.update(&b"late"[..]).is_err());
	}

	#[test]
	fn a_finished_track_ends_the_consumer() {
		let (mut producer, track) = producer(false);
		producer.update(&b"only"[..]).unwrap();
		producer.finish().unwrap();

		let mut consumer = consumer(track, false);
		let waiter = kio::Waiter::noop();
		assert!(matches!(consumer.poll_next(&waiter), Poll::Ready(Ok(Some(_)))));
		assert!(matches!(consumer.poll_next(&waiter), Poll::Ready(Ok(None))));
	}
}
