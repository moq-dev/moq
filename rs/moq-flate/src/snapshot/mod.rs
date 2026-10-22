//! Lossy latest-value opaque publishing over [`moq-net`](moq_net) tracks.
//!
//! One value updated over time. `Consumer::next` yields each state with its frame timestamp,
//! in order within the subscription's drift budget; `Consumer::latest` skips to the newest
//! state. Superseded groups may be lost, so use [`stream`](crate::stream) when every record
//! must survive. A playhead reader sets `max_delay` to its distance behind the live edge.
//!
//! On the wire each value is one group holding one frame, so a group is self-contained and a
//! consumer never needs an older one. With [`Config::compression`] set to
//! [`Compression::Deflate`], that frame is its own raw DEFLATE stream;
//! there is no window to share across a single-frame group.

use crate::Compression;
#[cfg(test)]
use futures::FutureExt;

pub mod consumer;
pub mod producer;

pub use consumer::Consumer;
pub use producer::Producer;

/// Codec options for a snapshot track.
///
/// Build from [`Default`] and override fields (the struct is `#[non_exhaustive]`, so new options
/// stay additive).
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct Config {
	/// Compress each value as its own raw DEFLATE stream.
	///
	/// A snapshot group holds a single self-contained value, so there is no window to share: each
	/// value is compressed alone. [`Compression::None`] (the default) writes the bytes through
	/// untouched. A [`Consumer`] must set the same [`compression`](Self::compression).
	pub compression: Compression,
}

#[cfg(test)]
mod test {
	use std::task::Poll;

	use bytes::Bytes;

	use super::*;

	/// Refused before a replacement group is published, so the last value stays readable.
	#[test]
	fn a_timed_track_refuses_a_bare_publish() {
		let (mut producer, track) = producer_on(false, timed());
		let at = moq_net::Timestamp::from_millis(100).unwrap();
		producer
			.update(moq_net::Timed::<Bytes>::from(&b"keep"[..]).at(at))
			.unwrap();
		assert!(matches!(
			producer.update(&b"bare"[..]),
			Err(crate::Error::Net(moq_net::Error::TimestampMismatch))
		));
		producer.finish().unwrap();
		assert_eq!(drain(consume(track, false)), vec![Bytes::from_static(b"keep")]);
	}

	#[test]
	fn a_bare_publish_stays_untimed() {
		let track = moq_net::broadcast::Info::new()
			.produce()
			.create_track("untimed", moq_net::track::Info::default())
			.unwrap();
		let mut groups = track.subscribe(None);
		let mut producer = Producer::new(track, Config::default());
		producer.update(&b"payload"[..]).unwrap();
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

	fn cfg(compression: bool) -> Config {
		Config {
			compression: if compression {
				Compression::Deflate
			} else {
				Compression::None
			},
		}
	}

	fn producer(compression: bool) -> (Producer, moq_net::track::Subscriber) {
		producer_on(compression, moq_net::track::Info::default())
	}

	fn producer_on(compression: bool, info: moq_net::track::Info) -> (Producer, moq_net::track::Subscriber) {
		let track = moq_net::broadcast::Info::new()
			.produce()
			.create_track("test", info)
			.unwrap();
		let consumer = track.subscribe(None);
		(Producer::new(track, cfg(compression)), consumer)
	}

	fn consume(track: moq_net::track::Subscriber, compression: bool) -> Consumer {
		Consumer::new(track, cfg(compression))
	}

	/// Drain newest states currently available without blocking.
	fn drain(mut consumer: Consumer) -> Vec<Bytes> {
		let waiter = kio::Waiter::noop();
		let mut out = Vec::new();
		while let Poll::Ready(Ok(Some(payload))) = consumer.poll_latest(&waiter) {
			out.push(payload.value);
		}
		out
	}

	#[test]
	fn next_preserves_intermediate_states() {
		let (mut producer, mut track) = producer(true);
		let mut subscription = track.subscription();
		subscription.max_delay = None;
		track.update(subscription).unwrap();
		for value in [b"one".as_slice(), b"two", b"three"] {
			producer.update(value).unwrap();
		}
		producer.finish().unwrap();
		let mut consumer = consume(track, true);
		let mut values = Vec::new();
		while let Poll::Ready(Ok(Some(value))) = consumer.poll_next(&kio::Waiter::noop()) {
			assert_eq!(value.at, None);
			values.push(value.value);
		}
		assert_eq!(
			values,
			vec![
				Bytes::from_static(b"one"),
				Bytes::from_static(b"two"),
				Bytes::from_static(b"three")
			]
		);
	}

	/// A snapshot group the transport can no longer serve -- `Old` when the relay reclaims a
	/// superseded group, `Evicted` under memory pressure, `Lagged` past the drift budget -- is not
	/// fatal. A snapshot reader only wants the newest value, so it drops the group and takes the
	/// replacement rather than ending the reader.
	#[test]
	fn a_lost_group_waits_for_its_replacement() {
		// Lost on this side, then reset by the publisher: the reader sees the same loss either way.
		for lost in [moq_net::Error::Old, moq_net::Error::Stream(moq_net::StreamError::Old)] {
			let track = moq_net::broadcast::Info::new()
				.produce()
				.create_track("test", timed())
				.unwrap();
			let mut consumer = consume(track.subscribe(None), false);
			let waiter = kio::Waiter::noop();

			// Group 0 delivers a value, then stays open with the reader parked on its next frame.
			let mut group = track.create_group(moq_net::group::Info { sequence: 0 }).unwrap();
			group
				.write_frame(moq_net::Timestamp::now(), Bytes::from_static(b"one"))
				.unwrap();
			assert!(matches!(consumer.poll_latest(&waiter), Poll::Ready(Ok(Some(v))) if v.value == "one"));
			assert!(consumer.poll_latest(&waiter).is_pending());

			// The group is lost out from under the reader.
			group.abort(lost).unwrap();
			assert!(
				consumer.poll_latest(&waiter).is_pending(),
				"a lost group must not end the reader"
			);

			// The replacement arrives and the reader picks up where the value now lives.
			let mut group = track.create_group(moq_net::group::Info { sequence: 1 }).unwrap();
			group
				.write_frame(moq_net::Timestamp::now(), Bytes::from_static(b"two"))
				.unwrap();
			assert!(matches!(consumer.poll_latest(&waiter), Poll::Ready(Ok(Some(v))) if v.value == "two"));
		}
	}

	/// A stamped payload is written at its capture time, and the returned size is the encoded frame
	/// on the wire rather than the payload handed in.
	#[test]
	fn a_stamped_update_writes_its_capture_time() {
		let (mut producer, track) = producer_on(true, timed());
		let mut groups = producer.consume();
		let captured = moq_net::Timestamp::from_millis(1_234).unwrap();
		let payload = Bytes::from(vec![7u8; 4096]);
		let size = producer
			.update(moq_net::Timed::from(payload.clone()).at(captured))
			.unwrap();

		let waiter = kio::Waiter::noop();
		let Poll::Ready(Ok(Some(mut group))) = groups.poll_recv_group(&waiter) else {
			panic!("expected a group");
		};
		let Poll::Ready(Ok(Some(frame))) = group.poll_read_frame(&waiter) else {
			panic!("expected a frame");
		};
		assert_eq!(frame.timestamp.unwrap().as_micros(), captured.as_micros());
		assert_eq!(size, frame.payload.len());
		assert!(size < payload.len(), "the size is the compressed frame");
		assert_eq!(drain(consume(track, true)), vec![payload]);
	}

	#[test]
	fn one_group_per_update() {
		let (mut producer, mut track) = producer(false);
		producer.update(&b"first"[..]).unwrap();
		producer.update(&b"second"[..]).unwrap();
		producer.finish().unwrap();

		// Two updates => two groups. A consumer that joins after both only sees the latest.
		assert_eq!(
			track.largest().now_or_never().flatten().map(|position| position.group),
			Some(1)
		);
		assert_eq!(drain(consume(track, false)), vec![Bytes::from_static(b"second")]);
	}

	#[test]
	fn live_consumer_sees_each_update() {
		let (mut producer, track) = producer(false);
		let mut consumer = consume(track, false);
		let waiter = kio::Waiter::noop();

		for n in 0..3u8 {
			producer.update(vec![n]).unwrap();
			match consumer.poll_latest(&waiter) {
				Poll::Ready(Ok(Some(payload))) => assert_eq!(&payload.value[..], &[n]),
				other => panic!("expected value, got {other:?}"),
			}
		}
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
	/// would vanish even though the update reported an error. Compression doesn't lift the limit: a
	/// value that inflates past the group budget is refused even though it deflates to a few KiB.
	#[test]
	fn a_rejected_update_leaves_the_previous_value_readable() {
		for compression in [false, true] {
			let (mut producer, track) = producer(compression);
			producer.update(&b"keep"[..]).unwrap();

			let oversized = Bytes::from(vec![0u8; moq_net::group::MAX_CACHE_BYTES as usize + 1]);
			assert!(matches!(
				producer.update(oversized),
				Err(crate::Error::Net(moq_net::Error::FrameTooLarge))
			));
			producer.finish().unwrap();

			// A reader arriving now still finds the last good value, not an empty superseding group.
			assert_eq!(drain(consume(track, compression)), vec![Bytes::from_static(b"keep")]);
		}
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

		let mut consumer = consume(track, false);
		let waiter = kio::Waiter::noop();
		assert!(matches!(consumer.poll_latest(&waiter), Poll::Ready(Ok(Some(_)))));
		assert!(matches!(consumer.poll_latest(&waiter), Poll::Ready(Ok(None))));
	}
}
