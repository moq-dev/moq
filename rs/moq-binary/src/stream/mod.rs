//! Lossless append-log binary publishing over [`moq-net`](moq_net) tracks.
//!
//! An ordered log of opaque payloads, for consumers that care about every one (an event log, a
//! sequence of samples). Nothing is ever superseded: a consumer yields each payload in the order it
//! was appended. For a latest-value document, use [`snapshot`](crate::snapshot) instead.
//!
//! Retention is bounded, which is the limit of "lossless" here. The group's cache is finite, so a
//! log longer than it holds evicts its earliest frames, and a consumer that falls behind or
//! subscribes late then fails its read ([`moq_net::Error::Lagged`]) rather than silently resuming
//! partway through: a partial log presented as a whole one is what this mode exists to prevent.
//! Keep a log inside what the group retains, and split anything unbounded across successive tracks.
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
		let (mut producer, track) = producer(true);
		let payload = Bytes::from(b"the quick brown fox".repeat(16));
		for _ in 0..8 {
			producer.append(payload.clone()).unwrap();
		}
		producer.finish().unwrap();

		let waiter = kio::Waiter::noop();
		let Poll::Ready(Ok(Some(mut group))) = track.ordered().poll_next_group(&waiter) else {
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

	/// A track whose timescale is extreme enough that converting a wall-clock timestamp into it
	/// overflows, so `write_frame` rejects every frame. That stands in for any post-`append_group`
	/// write failure (the real one is a frame over moq-net's 32 MB per-group cache) without
	/// allocating 32 MB to provoke it. Borrowed from moq-json's stream tests.
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
		let mut producer = Producer::new(track, ProducerConfig::default());

		assert!(producer.append(&b"rejected"[..]).is_err());

		// Aborting the group alone is not enough: the group is dropped from the cache and the reader
		// still sees a clean end, which is exactly what a completed log looks like.
		let waiter = kio::Waiter::noop();
		assert!(
			matches!(subscriber.poll_recv_group(&waiter), Poll::Ready(Err(_))),
			"a truncated log must surface an error rather than read as a completed one"
		);
	}

	/// A record the consumer could never decode is as lost as one the track rejects, so it takes the
	/// track with it rather than leaving a live log missing a record. Guards the guard: an early
	/// return here would bypass the terminal path and let a later append continue the gap.
	#[test]
	fn an_undecodable_record_ends_the_track() {
		let track = moq_net::broadcast::Info::new()
			.produce()
			.create_track("test", None)
			.unwrap();
		let mut subscriber = track.subscribe(None);
		let mut producer = Producer::new(track, ProducerConfig::default().with_compression(true));

		let oversized = Bytes::from(vec![0u8; moq_flate::DEFAULT_MAX_FRAME_SIZE as usize + 1]);
		assert!(matches!(
			producer.append(oversized),
			Err(crate::Error::Flate(moq_flate::Error::TooLarge(_)))
		));

		// Nothing was published, and the track is terminal rather than merely skipping the record.
		let waiter = kio::Waiter::noop();
		assert!(matches!(subscriber.poll_recv_group(&waiter), Poll::Ready(Err(_))));
		assert!(producer.append(&b"after"[..]).is_err());
	}

	/// A reader already inside the group keeps its own handle, which `track::Producer::abort`
	/// deliberately leaves independent. So the group has to carry the same error, or that reader is
	/// told the producer went away rather than why the log stopped.
	#[test]
	fn a_reader_inside_the_group_sees_the_real_error() {
		let track = moq_net::broadcast::Info::new()
			.produce()
			.create_track("test", None)
			.unwrap();
		let mut subscriber = track.subscribe(None);
		let mut producer = Producer::new(track, ProducerConfig::default());

		producer.append(&b"first"[..]).unwrap();

		// Pull the group, the way a live reader would, so we hold a handle of our own.
		let waiter = kio::Waiter::noop();
		let Poll::Ready(Ok(Some(mut group))) = subscriber.poll_recv_group(&waiter) else {
			panic!("the group was published, so a subscriber sees it");
		};
		assert!(matches!(group.poll_read_frame(&waiter), Poll::Ready(Ok(Some(_)))));

		// A payload the group cannot hold, so the write fails and ends the log.
		let oversized = Bytes::from(vec![0u8; moq_net::group::MAX_CACHE_BYTES as usize + 1]);
		assert!(producer.append(oversized).is_err());

		match group.poll_read_frame(&waiter) {
			Poll::Ready(Err(err)) => assert!(
				matches!(err, moq_net::Error::FrameTooLarge),
				"the reader should see the write's own error, got {err:?}"
			),
			other => panic!("expected the write error, got {other:?}"),
		}
	}

	/// These tests walk a finished multi-group track, so they need a subscriber that tolerates a
	/// backlog. The default budget is [`Duration::ZERO`], which abandons any group a newer one has
	/// already superseded.
	fn replaying() -> moq_net::track::Subscription {
		moq_net::track::Subscription::default().with_max_age(std::time::Duration::from_secs(30))
	}

	/// The track ends with the group, so nothing opens a second one and splits the log.
	#[test]
	fn a_failed_write_ends_the_track() {
		let track = rejecting_track();
		let mut producer = Producer::new(track, ProducerConfig::default());

		assert!(producer.append(&b"rejected"[..]).is_err());
		assert!(
			producer.append(&b"again"[..]).is_err(),
			"a second append must fail on the closed track rather than open another group"
		);
	}

	/// A stream is one group. A publisher that rolls to a second one lost whatever would have
	/// completed the first, so the read reports that rather than handing back the remainder as a
	/// continuous log. Written by hand because this producer never rolls.
	#[test]
	fn a_second_group_is_a_rolled_log() {
		let mut track = moq_net::broadcast::Info::new()
			.produce()
			.create_track("test", None)
			.unwrap();
		let subscriber = track.subscribe(replaying());

		for pair in payloads(4).chunks(2) {
			// Each group is its own DEFLATE stream, which is what a recovery roll would produce.
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

		let mut consumer = Consumer::new(subscriber, ConsumerConfig::default().with_compression(true));
		let waiter = kio::Waiter::noop();

		// The log's one group reads normally.
		assert!(matches!(consumer.poll_next(&waiter), Poll::Ready(Ok(Some(_)))));
		assert!(matches!(consumer.poll_next(&waiter), Poll::Ready(Ok(Some(_)))));

		// The second group is a gap, not a continuation.
		assert!(matches!(
			consumer.poll_next(&waiter),
			Poll::Ready(Err(crate::Error::Rolled))
		));
	}

	/// Groups are separate QUIC streams, so a second one can land before the first. Reading in
	/// arrival order is what catches that: the monotonic `next_group` would skip the late lower
	/// sequence and end the log cleanly, reporting a truncated log as a whole one.
	#[test]
	fn a_late_lower_group_is_still_reported() {
		let mut track = moq_net::broadcast::Info::new()
			.produce()
			.create_track("test", None)
			.unwrap();
		let subscriber = track.subscribe(replaying());

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

		let mut consumer = Consumer::new(subscriber, ConsumerConfig::default().with_compression(true));
		let waiter = kio::Waiter::noop();

		assert!(matches!(
			consumer.poll_next(&waiter),
			Poll::Ready(Ok(Some(payload))) if payload == vec![1u8; 8]
		));
		assert!(matches!(
			consumer.poll_next(&waiter),
			Poll::Ready(Err(crate::Error::Rolled))
		));
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
