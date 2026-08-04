//! Lossy latest-value JSON publishing over [`moq-net`](moq_net) tracks.
//!
//! One JSON value updated over time, for consumers that only care about the current state (a
//! catalog, a status document). This mode is **lossy** by design: a consumer yields only the
//! most recent value. A late joiner (or a consumer that falls behind) jumps straight to the
//! newest group and collapses any buffered backlog into a single yield, and older groups are
//! dropped entirely. Intermediate updates are never replayed. For an ordered log where every
//! record is preserved, use [`stream`](crate::stream) instead.
//!
//! On the wire the value is published as a series of groups, where each group is
//! self-contained: its first frame is a full snapshot and any following frames are
//! [RFC 7396](https://www.rfc-editor.org/rfc/rfc7396.html) JSON Merge Patch deltas applied in
//! order. A consumer jumps to the newest group, reads the snapshot, and applies the deltas, so
//! a late joiner never needs older groups.
//!
//! Deltas are controlled by [`ProducerConfig::delta_ratio`]. A ratio of `0` disables them, so every
//! change is a fresh snapshot group, matching a plain "one JSON blob per group" track.
//!
//! # Choosing a layer
//!
//! [`Producer`] and [`Consumer`] own a track: hand one a
//! [`track::Producer`](moq_net::track::Producer) and it manages the groups for you.
//!
//! [`Encoder`] and [`Decoder`] are the same logic without the track. The encoder turns values into
//! [`Encoded`] frame payloads and says where the group boundaries fall; the decoder reconstructs a
//! value from those payloads. Reach for them when something else is already in charge of the track,
//! for example a `moq_mux::container::Producer` also managing a timeline and a catalog estimate.
//! [`Encoded::keyframe`] maps straight onto `moq_mux::container::Frame::keyframe`.

mod consumer;
mod decoder;
mod encoder;
mod producer;

pub use consumer::Consumer;
pub use decoder::{ConsumerConfig, Decoder};
pub use encoder::{Encoded, Encoder, ProducerConfig};
pub use producer::{Guard, Producer};

#[cfg(test)]
mod test {
	use std::task::Poll;

	use bytes::Bytes;
	use serde_json::{Value, json};

	use super::encoder::MAX_DELTA_FRAMES;
	use super::*;

	/// An uncompressed config with the given delta ratio.
	fn cfg(delta_ratio: u32) -> ProducerConfig {
		ProducerConfig::default().with_delta_ratio(delta_ratio)
	}

	/// A DEFLATE-compressed config with the given delta ratio.
	fn cfg_deflate(delta_ratio: u32) -> ProducerConfig {
		cfg(delta_ratio).with_compression(true)
	}

	/// A consumer reading compressed frames.
	fn deflate_consumer(track: moq_net::track::Subscriber) -> Consumer<Value> {
		Consumer::new(track, ConsumerConfig::default().with_compression(true))
	}

	fn producer(config: ProducerConfig) -> (Producer<Value>, moq_net::track::Subscriber) {
		let track = moq_net::broadcast::Info::new()
			.produce()
			.create_track("test", None)
			.unwrap();
		let consumer = track.subscribe(None);
		(Producer::new(track, config), consumer)
	}

	/// Drain every value currently available from a plaintext consumer without blocking.
	fn drain(track: moq_net::track::Subscriber) -> Vec<Value> {
		drain_with(Consumer::<Value>::new(track, ConsumerConfig::default()))
	}

	/// Drain every value currently available from an already-built consumer without blocking.
	fn drain_with(mut consumer: Consumer<Value>) -> Vec<Value> {
		let waiter = kio::Waiter::noop();
		let mut out = Vec::new();
		while let Poll::Ready(Ok(Some(value))) = consumer.poll_next(&waiter) {
			out.push(value);
		}
		out
	}

	#[test]
	fn deltas_off_snapshot_per_group() {
		let (mut producer, track) = producer(cfg(0));
		producer.update(&json!({ "a": 1 })).unwrap();
		producer.update(&json!({ "a": 2 })).unwrap();
		producer.finish().unwrap();

		// Two updates => two groups, each a full snapshot. A consumer that joins after both
		// exist only sees the latest, like the existing catalog consumer.
		assert_eq!(track.latest(), Some(1));
		assert_eq!(drain(track), vec![json!({ "a": 2 })]);
	}

	#[test]
	fn live_consumer_sees_each_update() {
		let (mut producer, track) = producer(ProducerConfig::default());
		let mut consumer = Consumer::<Value>::new(track, ConsumerConfig::default());
		let waiter = kio::Waiter::noop();

		for n in 1..=3 {
			producer.update(&json!({ "a": n })).unwrap();
			match consumer.poll_next(&waiter) {
				Poll::Ready(Ok(Some(value))) => assert_eq!(value, json!({ "a": n })),
				other => panic!("expected value, got {other:?}"),
			}
		}
	}

	#[test]
	fn unchanged_value_writes_nothing() {
		let (mut producer, track) = producer(ProducerConfig::default());
		producer.update(&json!({ "a": 1 })).unwrap();
		producer.update(&json!({ "a": 1 })).unwrap();
		producer.finish().unwrap();

		assert_eq!(track.latest(), Some(0));
		assert_eq!(drain(track), vec![json!({ "a": 1 })]);
	}

	#[test]
	fn deltas_share_one_group() {
		let (mut producer, track) = producer(cfg(100));
		producer.update(&json!({ "a": 1, "b": 1 })).unwrap();
		producer.update(&json!({ "a": 1, "b": 2 })).unwrap();
		producer.update(&json!({ "a": 1, "b": 3 })).unwrap();
		producer.finish().unwrap();

		// All updates fit in a single group as snapshot + deltas.
		assert_eq!(track.latest(), Some(0));
		let values = drain(track);
		assert_eq!(values.last().unwrap(), &json!({ "a": 1, "b": 3 }));
	}

	#[test]
	fn tight_ratio_rolls_snapshots() {
		// A ratio of 1 budgets deltas up to one snapshot (equal 7-byte frames => 7 bytes). The gate
		// checks the deltas already written, so the delta that tips the group over budget still lands
		// (a one-frame overshoot): group 0 takes two deltas (14 bytes) before the fourth update rolls
		// group 1. (Still distinct from 0, which disables deltas entirely.)
		let (mut producer, track) = producer(cfg(1));
		producer.update(&json!({ "a": 1 })).unwrap(); // snapshot, group 0
		producer.update(&json!({ "a": 2 })).unwrap(); // delta, group 0 (deltas = 7)
		producer.update(&json!({ "a": 3 })).unwrap(); // delta, group 0 (deltas = 14, now over budget)
		producer.update(&json!({ "a": 4 })).unwrap(); // budget already exceeded, rolls group 1
		producer.finish().unwrap();

		assert_eq!(track.latest(), Some(1));
	}

	#[test]
	fn deltas_stay_within_ratio_times_snapshot() {
		// The budget covers only the deltas, not the snapshot frame, measured against the group's
		// snapshot size. Single-digit values keep every frame at a constant 7 bytes (`{"n":N}`), so
		// `ratio = 8` budgets 56 bytes of deltas. The gate checks the deltas already written, so the
		// group keeps filling until the accumulated deltas first exceed 56 (nine deltas = 63 bytes) and
		// the next update rolls (a one-frame overshoot past the 56-byte budget).
		let (mut producer, track) = producer(cfg(8));
		for n in 0..=10 {
			producer.update(&json!({ "n": n })).unwrap();
		}
		producer.finish().unwrap();

		// Group 0 carries the snapshot plus 9 deltas (10 frames); the 10th delta opens group 1.
		assert_eq!(track.latest(), Some(1));
		assert_eq!(drain(track).last().unwrap(), &json!({ "n": 10 }));
	}

	#[test]
	fn array_change_is_delta() {
		let (mut producer, track) = producer(cfg(100));
		producer.update(&json!({ "list": [1, 2] })).unwrap();
		producer.update(&json!({ "list": [1, 2, 3] })).unwrap();
		producer.finish().unwrap();

		// The array is replaced wholesale in a delta, so it stays in the same group.
		assert_eq!(track.latest(), Some(0));
		assert_eq!(drain(track).last().unwrap(), &json!({ "list": [1, 2, 3] }));
	}

	#[test]
	fn frame_cap_rolls_snapshot() {
		let (mut producer, track) = producer(cfg(1_000_000));
		// First update is the snapshot (frame 0); then MAX_DELTA_FRAMES - 1 deltas fill the group.
		for i in 0..=MAX_DELTA_FRAMES {
			producer.update(&json!({ "n": i })).unwrap();
		}
		producer.finish().unwrap();

		// The frame cap forced exactly one extra snapshot group despite the huge ratio.
		assert_eq!(track.latest(), Some(1));
		assert_eq!(drain(track).last().unwrap(), &json!({ "n": MAX_DELTA_FRAMES }));
	}

	#[test]
	fn late_joiner_reconstructs_from_deltas() {
		let (mut producer, track) = producer(cfg(100));
		producer.update(&json!({ "a": 1, "b": 1 })).unwrap();
		producer.update(&json!({ "a": 1, "b": 2 })).unwrap();
		producer.update(&json!({ "a": 5, "b": 2 })).unwrap();
		producer.finish().unwrap();

		// A consumer created only now still rebuilds the final value from snapshot + deltas.
		assert_eq!(drain(track).last().unwrap(), &json!({ "a": 5, "b": 2 }));
	}

	#[test]
	fn lock_composes_independent_owners() {
		// Mirrors the catalog use case: separate owners each edit their own field through the guard.
		#[derive(serde::Serialize, serde::Deserialize, Default, PartialEq, Debug)]
		struct Doc {
			#[serde(skip_serializing_if = "Option::is_none")]
			video: Option<String>,
			#[serde(skip_serializing_if = "Option::is_none")]
			scte35: Option<u32>,
		}

		let track = moq_net::broadcast::Info::new()
			.produce()
			.create_track("test", None)
			.unwrap();
		let consumer = track.subscribe(None);
		let mut producer = Producer::<Doc>::new(track, ProducerConfig::default());

		// First owner sets its field.
		producer.lock().video = Some("v1".to_string());

		// Second owner starts from the latest value and adds its own field without clobbering.
		producer.lock().scte35 = Some(42);

		// Locking without mutating publishes nothing (the guard stays clean).
		let _ = producer.lock();

		producer.finish().unwrap();

		let mut consumer = Consumer::<Doc>::new(consumer, ConsumerConfig::default());
		let waiter = kio::Waiter::noop();
		let mut last = None;
		while let Poll::Ready(Ok(Some(value))) = consumer.poll_next(&waiter) {
			last = Some(value);
		}
		assert_eq!(
			last.unwrap(),
			Doc {
				video: Some("v1".to_string()),
				scte35: Some(42),
			}
		);
	}

	#[test]
	fn commit_reports_a_publish_failure() {
		#[derive(serde::Serialize, serde::Deserialize, Default, PartialEq, Debug)]
		struct Doc {
			a: u32,
		}

		let track = moq_net::broadcast::Info::new()
			.produce()
			.create_track("test", None)
			.unwrap();
		let mut producer = Producer::<Doc>::new(track, ProducerConfig::default());

		// A finished track can't take another group, so the publish behind the guard fails.
		producer.finish().unwrap();

		let mut guard = producer.lock();
		guard.a = 1;
		assert!(matches!(guard.commit(), Err(crate::Error::Net(_))));
	}

	#[test]
	fn commit_publishes_once() {
		#[derive(serde::Serialize, serde::Deserialize, Default, PartialEq, Debug)]
		struct Doc {
			a: u32,
		}

		let track = moq_net::broadcast::Info::new()
			.produce()
			.create_track("test", None)
			.unwrap();
		let consumer = track.subscribe(None);
		let mut producer = Producer::<Doc>::new(track, cfg(0));

		let mut guard = producer.lock();
		guard.a = 1;
		guard.commit().unwrap();

		// The drop that follows `commit` must not publish a second group.
		producer.finish().unwrap();
		assert_eq!(consumer.latest(), Some(0));
	}

	#[test]
	fn newer_group_supersedes_in_progress_reconstruction() {
		// A tight ratio fills group 0 with a couple of deltas, then forces a later update into a new
		// snapshot group (the gate overshoots the budget by one delta before rolling).
		let (mut producer, track) = producer(cfg(1));
		let observer = producer.consume();
		let mut consumer = Consumer::<Value>::new(track, ConsumerConfig::default());
		let waiter = kio::Waiter::noop();

		producer.update(&json!({ "a": 1 })).unwrap(); // snapshot, group 0
		match consumer.poll_next(&waiter) {
			Poll::Ready(Ok(Some(value))) => assert_eq!(value, json!({ "a": 1 })),
			other => panic!("expected first value, got {other:?}"),
		}

		producer.update(&json!({ "a": 2 })).unwrap(); // delta in group 0 (deltas = 7)
		producer.update(&json!({ "a": 3 })).unwrap(); // delta in group 0 (deltas = 14, now over budget)
		producer.update(&json!({ "a": 4 })).unwrap(); // budget already exceeded, rolls group 1
		producer.finish().unwrap();
		assert_eq!(observer.latest(), Some(1));

		// The consumer jumps to the newest group and never yields a stale value.
		let mut last = None;
		while let Poll::Ready(Ok(Some(value))) = consumer.poll_next(&waiter) {
			last = Some(value);
		}
		assert_eq!(last.unwrap(), json!({ "a": 4 }));
	}

	#[test]
	fn open_group_pends_after_track_finish() {
		// A group appended before the track finishes may still deliver frames, so the consumer must
		// keep waiting on it rather than ending the stream. Regression for the backlog-collapse poll.
		let mut track = moq_net::broadcast::Info::new()
			.produce()
			.create_track("test", None)
			.unwrap();
		let mut group = track.append_group().unwrap();
		let consumer_track = track.subscribe(None);
		track.finish().unwrap();

		let mut consumer = Consumer::<Value>::new(consumer_track, ConsumerConfig::default());
		let waiter = kio::Waiter::noop();

		// Track is finished but the open group is empty: pending, not end-of-stream.
		assert!(matches!(consumer.poll_next(&waiter), Poll::Pending));

		group
			.write_frame(
				moq_net::Timestamp::ZERO,
				Bytes::from(serde_json::to_vec(&json!({ "a": 1 })).unwrap()),
			)
			.unwrap();
		group.finish().unwrap();

		match consumer.poll_next(&waiter) {
			Poll::Ready(Ok(Some(value))) => assert_eq!(value, json!({ "a": 1 })),
			other => panic!("expected the catalog value, got {other:?}"),
		}
	}

	#[test]
	fn late_joiner_collapses_backlog_to_latest() {
		// A whole group's worth of snapshot + deltas is buffered before the consumer reads. It should
		// apply them all but yield only the latest value once, not replay every superseded state.
		let (mut producer, track) = producer(cfg(100));
		for n in 0..=20 {
			producer.update(&json!({ "n": n })).unwrap();
		}
		producer.finish().unwrap();

		// One group (ratio is generous), so a single poll drains the backlog into one yield.
		assert_eq!(track.latest(), Some(0));
		let values = drain(track);
		assert_eq!(
			values,
			vec![json!({ "n": 20 })],
			"backlog should collapse to the latest value"
		);
	}

	#[test]
	fn compressed_late_joiner_collapses_backlog_to_latest() {
		// Same collapse, exercising the lazy decoder replaying the group's slices to warm its window.
		let (mut producer, track) = producer(cfg_deflate(100));
		for n in 0..=20 {
			producer.update(&json!({ "n": n })).unwrap();
		}
		producer.finish().unwrap();

		assert_eq!(track.latest(), Some(0));
		let values = drain_with(deflate_consumer(track));
		assert_eq!(
			values,
			vec![json!({ "n": 20 })],
			"compressed backlog should collapse to the latest"
		);
	}

	#[test]
	fn compressed_snapshot_per_group_roundtrips() {
		let (mut producer, track) = producer(cfg_deflate(0));
		producer.update(&json!({ "a": 1 })).unwrap();
		producer.update(&json!({ "a": 2 })).unwrap();
		producer.finish().unwrap();

		// Deltas disabled: one compressed snapshot per group, latest reconstructs identically.
		assert_eq!(track.latest(), Some(1));
		let values = drain_with(deflate_consumer(track));
		assert_eq!(values, vec![json!({ "a": 2 })]);
	}

	#[test]
	fn compressed_deltas_share_one_group() {
		let (mut producer, track) = producer(cfg_deflate(100));
		producer.update(&json!({ "a": 1, "b": 1 })).unwrap();
		producer.update(&json!({ "a": 1, "b": 2 })).unwrap();
		producer.update(&json!({ "a": 1, "b": 3 })).unwrap();
		producer.finish().unwrap();

		// Snapshot + deltas in one group, each frame decompressed against the shared window.
		assert_eq!(track.latest(), Some(0));
		let values = drain_with(deflate_consumer(track));
		assert_eq!(values.last().unwrap(), &json!({ "a": 1, "b": 3 }));
	}

	#[test]
	fn compressed_late_joiner_reconstructs_from_deltas() {
		let (mut producer, track) = producer(cfg_deflate(100));
		producer.update(&json!({ "a": 1, "b": 1 })).unwrap();
		producer.update(&json!({ "a": 1, "b": 2 })).unwrap();
		producer.update(&json!({ "a": 5, "b": 2 })).unwrap();
		producer.finish().unwrap();

		// A consumer created only now rebuilds the final value from the compressed snapshot + deltas.
		let values = drain_with(deflate_consumer(track));
		assert_eq!(values.last().unwrap(), &json!({ "a": 5, "b": 2 }));
	}

	#[test]
	fn compressed_deltas_roll_on_compressed_budget() {
		// With compression the budget is measured on compressed frame sizes: `snapshot_len` and
		// `delta_bytes` are the compressed slice lengths, not the raw JSON. A tight ratio over many
		// distinct updates must therefore roll at least one group, and a late joiner must still rebuild
		// the final value across the compressed group boundary (per-group decoder reset). Guards against
		// the budget regressing to raw lengths.
		let (mut producer, track) = producer(cfg_deflate(2));
		for n in 0..=40 {
			producer.update(&json!({ "n": n })).unwrap();
		}
		producer.finish().unwrap();

		assert!(
			track.latest().unwrap() > 0,
			"a tight ratio should roll at least one compressed group"
		);
		assert_eq!(drain_with(deflate_consumer(track)).last().unwrap(), &json!({ "n": 40 }));
	}

	#[test]
	fn compression_shrinks_wire_frames() {
		// A repetitive payload should serialize to fewer wire bytes compressed than plaintext.
		let value = json!({ "renditions": ["video".repeat(50), "video".repeat(50), "video".repeat(50)] });

		let plaintext_bytes = wire_frame_len(cfg(0), &value);
		let compressed_bytes = wire_frame_len(cfg_deflate(0), &value);
		assert!(
			compressed_bytes < plaintext_bytes,
			"compressed frame {compressed_bytes} should be smaller than plaintext {plaintext_bytes}"
		);
	}

	#[test]
	fn compressed_deltas_reuse_window() {
		// The shared per-group window is the whole point: a delta that restates content already in
		// the snapshot compresses to far fewer bytes than the raw patch.
		let (mut producer, mut track) = producer(cfg_deflate(100));
		let phrase = "Media over QUIC delivers real-time latency at massive scale";
		producer.update(&json!({ "note": phrase })).unwrap();
		producer.update(&json!({ "note": phrase, "echo": phrase })).unwrap();
		producer.finish().unwrap();

		// Both frames land in group 0; read the delta (frame 1) verbatim.
		let waiter = kio::Waiter::noop();
		let Poll::Ready(Ok(Some(mut group))) = track.poll_next_group(&waiter) else {
			panic!("expected a group");
		};
		let mut frames = Vec::new();
		while let Poll::Ready(Ok(Some(frame))) = group.poll_read_frame(&waiter) {
			frames.push(frame.payload);
		}
		assert_eq!(frames.len(), 2, "snapshot + one delta in a single group");

		// The raw patch repeats the whole phrase; compressed against the window it's a fraction.
		let raw_delta = serde_json::to_vec(&json!({ "echo": phrase })).unwrap();
		assert!(
			frames[1].len() < raw_delta.len() / 2,
			"windowed delta {} should be far below the raw patch {}",
			frames[1].len(),
			raw_delta.len()
		);
	}

	#[test]
	fn rejected_field_names_its_path() {
		#[derive(serde::Deserialize)]
		#[allow(dead_code)]
		struct Inner {
			count: u8,
		}
		#[derive(serde::Deserialize)]
		#[allow(dead_code)]
		struct Outer {
			inner: Inner,
		}

		let (mut producer, track) = producer(cfg(0));
		producer.update(&json!({ "inner": { "count": 300 } })).unwrap();

		let mut consumer = Consumer::<Outer>::new(track, ConsumerConfig::default());
		let Poll::Ready(Err(err)) = consumer.poll_next(&kio::Waiter::noop()) else {
			panic!("expected a deserialize error");
		};

		// Deserializing from a Value has no line/column, so the path is the only locator a
		// consumer gets. See https://github.com/moq-dev/moq/issues/2509.
		assert!(err.to_string().starts_with("json: inner.count: "), "{err}");
	}

	#[test]
	fn rejected_root_omits_the_path() {
		let (mut producer, track) = producer(cfg(0));
		producer.update(&json!("not a map")).unwrap();

		let mut consumer = Consumer::<std::collections::BTreeMap<String, u8>>::new(track, ConsumerConfig::default());
		let Poll::Ready(Err(err)) = consumer.poll_next(&kio::Waiter::noop()) else {
			panic!("expected a deserialize error");
		};
		assert_eq!(
			err.to_string(),
			"json: invalid type: string \"not a map\", expected a map"
		);
	}

	/// Publish a single value and return the byte length of the resulting (frame 0) wire frame.
	fn wire_frame_len(config: ProducerConfig, value: &Value) -> usize {
		let (mut producer, mut track) = producer(config);
		producer.update(value).unwrap();
		producer.finish().unwrap();

		let waiter = kio::Waiter::noop();
		let Poll::Ready(Ok(Some(mut group))) = track.poll_next_group(&waiter) else {
			panic!("expected a group");
		};
		// Read the stored (possibly compressed) frame bytes verbatim, without reconstructing JSON.
		let Poll::Ready(Ok(Some(frame))) = group.poll_read_frame(&waiter) else {
			panic!("expected a frame");
		};
		frame.payload.len()
	}
}
