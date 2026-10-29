//! Lossy latest-value JSON publishing over [`moq-net`](moq_net) tracks.
//!
//! One value updated over time. `Consumer::next` yields each state with its frame timestamp,
//! in order within the subscription's drift budget; `Consumer::latest` skips to the newest
//! state. Superseded groups may be lost, so use [`stream`](crate::stream) when every record
//! must survive. A playhead reader sets `max_delay` to its distance behind the live edge.
//!
//! On the wire the value is published as a series of groups, where each group is
//! self-contained: its first frame is a full snapshot and any following frames are
//! [RFC 7396](https://www.rfc-editor.org/rfc/rfc7396.html) JSON Merge Patch deltas applied in
//! order. Every group starts independently, so a reader can resynchronize after skipping
//! superseded groups without needing earlier deltas.
//!
//! [`Config::deltas`] turns deltas off, so every change is a fresh snapshot group, matching a plain
//! "one JSON blob per group" track.
//!
//! # Refresh
//!
//! A group of deltas grows until a joiner wants a fresh one. A track created with a
//! [`refresh`](moq_net::track::Info::refresh) ratio asks for one when a subscriber joins while the
//! open group's deltas outweigh that ratio times its snapshot. The joiner still reads the cached
//! group meanwhile: the ratio limits how often groups roll, not what a joiner downloads.
//! [`Producer::refresh`] answers by restating the value as a snapshot in
//! a new group; [`Encoder::snapshot`] does the same for a caller that owns the track. Without a ratio,
//! or behind a relay that cannot forward the ask, a group runs to the encoder's frame and byte caps.
//!
//! A caller can also roll a group for its own reasons with [`Producer::cut`]: it closes the open
//! group and leaves the next update to open the replacement with a full snapshot, so the deltas
//! already written stop being provisional without publishing an empty group.
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
//!
//! Encoding advances state that the frame's consumers depend on, so [`Encoder::update`] hands back a
//! [`Pending`] the caller commits once the write succeeds. Dropping it uncommitted resynchronizes
//! the encoder, which keeps a frame that never reached the wire from desyncing the stream.

pub mod consumer;
#[cfg(test)]
use futures::FutureExt;
mod decoder;
mod encoder;
pub mod producer;

pub use consumer::Consumer;
pub use decoder::Decoder;
pub use encoder::{Config, Encoded, Encoder, Pending};
pub use producer::{Guard, Producer};

#[cfg(test)]
mod test {
	use std::task::Poll;

	use bytes::Bytes;
	use serde_json::{Value, json};

	use super::encoder::MAX_DELTA_FRAMES;
	use super::*;

	#[test]
	fn editing_keeps_the_callers_timestamp_on_commit_and_drop() {
		let (mut producer, _) = producer_on(cfg(true), timed());
		let mut groups = producer.consume();
		let first = moq_net::Timestamp::from_millis(100).unwrap();
		let second = moq_net::Timestamp::from_millis(200).unwrap();
		producer.modify(Some(first)).unwrap()["n"] = json!(1);
		let mut edit = producer.modify(Some(second)).unwrap();
		edit["n"] = json!(2);
		edit.commit().unwrap();
		let waiter = kio::Waiter::noop();
		let Poll::Ready(Ok(Some(mut group))) = groups.poll_recv_group(&waiter) else {
			panic!("expected the edited snapshot");
		};
		for at in [first, second] {
			let Poll::Ready(Ok(Some(frame))) = group.poll_read_frame(&waiter) else {
				panic!("expected the edited value");
			};
			assert_eq!(frame.timestamp, Some(at));
		}
	}

	#[test]
	fn a_missing_edit_timestamp_is_refused_on_a_timed_track() {
		let (mut producer, _) = producer_on(cfg(false), timed());
		let mut edit = producer.modify(None).unwrap();
		edit["n"] = json!(1);
		assert!(matches!(
			edit.commit(),
			Err(crate::Error::Net(moq_net::Error::TimestampMismatch))
		));
		let at = moq_net::Timestamp::from_millis(100).unwrap();
		producer.mutate(|value| value["n"] = json!(2), Some(at)).unwrap();
		let waiter = kio::Waiter::noop();
		let mut groups = producer.consume();
		let Poll::Ready(Ok(Some(mut group))) = groups.poll_recv_group(&waiter) else {
			panic!("expected the subsequent explicit publish");
		};
		let Poll::Ready(Ok(Some(frame))) = group.poll_read_frame(&waiter) else {
			panic!("expected a timed frame");
		};
		assert_eq!(frame.timestamp, Some(at));
	}

	/// Refused before a replacement group is published, so the last value stays readable.
	#[test]
	fn a_timed_track_refuses_a_bare_publish() {
		for deltas in [false, true] {
			let (mut producer, track) = producer_on(cfg(deltas), timed());
			let at = moq_net::Timestamp::from_millis(100).unwrap();
			producer
				.update(moq_net::Timed::from(&json!({ "n": 1 })).at(at))
				.unwrap();
			assert!(matches!(
				producer.update(&json!({ "n": 2 })),
				Err(crate::Error::Net(moq_net::Error::TimestampMismatch))
			));
			producer.finish().unwrap();
			assert_eq!(drain(track), vec![json!({ "n": 1 })], "deltas={deltas}");
		}
	}

	#[test]
	fn a_bare_publish_stays_untimed() {
		let track = moq_net::broadcast::Info::new()
			.produce()
			.create_track("untimed", moq_net::track::Info::default())
			.unwrap();
		let mut groups = track.subscribe(None);
		let mut producer = Producer::new(track, Config::default());
		producer.update(&json!({ "n": 1 })).unwrap();
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

	/// A config with or without deltas.
	fn cfg(deltas: bool) -> Config {
		Config::default().with_deltas(deltas)
	}

	/// A compressed track.
	fn compressed() -> moq_net::track::Info {
		moq_net::track::Info::default().with_compress(true)
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

	/// Drain newest states currently available from a plaintext consumer without blocking.
	fn drain(track: moq_net::track::Subscriber) -> Vec<Value> {
		drain_with(Consumer::<Value>::new(track, consumer::Config::default()))
	}

	/// Drain newest states currently available from an already-built consumer without blocking.
	fn drain_with(mut consumer: Consumer<Value>) -> Vec<Value> {
		let waiter = kio::Waiter::noop();
		let mut out = Vec::new();
		while let Poll::Ready(Ok(Some(value))) = consumer.poll_latest(&waiter) {
			out.push(value.value);
		}
		out
	}

	#[test]
	fn next_preserves_intermediate_states() {
		let (mut producer, mut track) = producer(cfg(true));
		let mut subscription = track.subscription();
		subscription.max_delay = None;
		track.update(subscription).unwrap();
		for n in [1, 2, 3] {
			producer
				.update(&json!({ "padding": "same-long-value-so-deltas-stay-in-one-group", "n": n }))
				.unwrap();
		}
		producer.finish().unwrap();
		let mut consumer = Consumer::<Value>::new(track, consumer::Config::default());
		let mut values = Vec::new();
		while let Poll::Ready(Ok(Some(value))) = consumer.poll_next(&kio::Waiter::noop()) {
			assert_eq!(value.at, None);
			values.push(value.value["n"].as_u64().unwrap());
		}
		assert_eq!(values, vec![1, 2, 3]);
	}

	/// A snapshot group the transport can no longer serve -- `Old` when the relay reclaims a
	/// superseded group, `Evicted` under memory pressure, `Lagged` past the drift budget -- is not
	/// fatal. A snapshot reader only wants the newest value, so it drops the group and takes the
	/// replacement. Regression test for `moq export ts` exiting on `Error: json: old` when a
	/// catalog group aged out of the relay cache underneath it.
	#[test]
	fn a_lost_group_waits_for_its_replacement() {
		// Lost on this side, then reset by the publisher: the reader sees the same loss either way.
		for lost in [moq_net::Error::Old, moq_net::Error::Stream(moq_net::StreamError::Old)] {
			let track = moq_net::broadcast::Info::new()
				.produce()
				.create_track("test", timed())
				.unwrap();
			let mut consumer = Consumer::<Value>::new(track.subscribe(None), consumer::Config::default());
			let waiter = kio::Waiter::noop();

			// Group 0 delivers a value, then stays open with the reader parked on its next frame.
			let mut group = track.create_group(moq_net::group::Info { sequence: 0 }).unwrap();
			group
				.write_frame(moq_net::Timestamp::now(), Bytes::from_static(br#"{"a":1}"#))
				.unwrap();
			assert!(matches!(consumer.poll_latest(&waiter), Poll::Ready(Ok(Some(v))) if v.value == json!({ "a": 1 })));
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
				.write_frame(moq_net::Timestamp::now(), Bytes::from_static(br#"{"a":2}"#))
				.unwrap();
			assert!(matches!(consumer.poll_latest(&waiter), Poll::Ready(Ok(Some(v))) if v.value == json!({ "a": 2 })));
		}
	}

	/// Nothing replaces a lost group once the track is finished, so the reader ends cleanly on the
	/// last value it reconstructed instead of reporting the eviction as a failure.
	#[test]
	fn a_lost_group_on_a_finished_track_ends_cleanly() {
		let track = moq_net::broadcast::Info::new()
			.produce()
			.create_track("test", timed())
			.unwrap();
		let mut consumer = Consumer::<Value>::new(track.subscribe(None), consumer::Config::default());
		let waiter = kio::Waiter::noop();

		let mut group = track.create_group(moq_net::group::Info { sequence: 0 }).unwrap();
		group
			.write_frame(moq_net::Timestamp::now(), Bytes::from_static(br#"{"a":1}"#))
			.unwrap();
		assert!(matches!(consumer.poll_latest(&waiter), Poll::Ready(Ok(Some(v))) if v.value == json!({ "a": 1 })));

		group.abort(moq_net::Error::Old).unwrap();
		track.finish().unwrap();
		assert!(matches!(consumer.poll_latest(&waiter), Poll::Ready(Ok(None))));
	}

	#[test]
	fn a_cut_makes_the_next_update_a_snapshot_group() {
		let (mut producer, mut track) = producer(cfg(true));
		producer.update(&json!({ "a": 1, "b": 1 })).unwrap();
		producer.update(&json!({ "a": 1, "b": 2 })).unwrap();
		producer.cut().unwrap();
		producer.update(&json!({ "a": 1, "b": 3 })).unwrap();
		producer.finish().unwrap();

		// The ratio would have kept every update in one group; the cut rolled it anyway. A consumer
		// joining at the new group reads the whole value from its first frame, with none of the deltas
		// that preceded the cut.
		assert_eq!(
			track.largest().now_or_never().flatten().map(|position| position.group),
			Some(1)
		);
		assert_eq!(drain(track).last().unwrap(), &json!({ "a": 1, "b": 3 }));
	}

	#[test]
	fn a_cut_republishes_an_unchanged_value() {
		let (mut producer, mut track) = producer(cfg(true));
		producer.update(&json!({ "a": 1 })).unwrap();
		producer.cut().unwrap();

		// An unchanged value normally writes nothing. After a cut it must still open the replacement
		// group, or the value would only exist in a group no new consumer reads.
		producer.update(&json!({ "a": 1 })).unwrap();
		producer.finish().unwrap();

		assert_eq!(
			track.largest().now_or_never().flatten().map(|position| position.group),
			Some(1)
		);
		assert_eq!(drain(track), vec![json!({ "a": 1 })]);
	}

	#[test]
	fn a_cut_opens_no_replacement_group() {
		let (mut producer, mut track) = producer(cfg(true));
		producer.update(&json!({ "a": 1 })).unwrap();
		producer.cut().unwrap();
		producer.finish().unwrap();

		// Cutting closes the open group and stops there: no empty group for a consumer to advance into
		// and wait on.
		assert_eq!(
			track.largest().now_or_never().flatten().map(|position| position.group),
			Some(0)
		);
		assert_eq!(drain(track), vec![json!({ "a": 1 })]);
	}

	#[test]
	fn a_cut_is_idempotent() {
		let (mut producer, mut track) = producer(cfg(true));

		// Nothing published yet, so there is no group to cut.
		producer.cut().unwrap();
		producer.cut().unwrap();
		producer.update(&json!({ "a": 1 })).unwrap();
		assert_eq!(
			track.largest().now_or_never().flatten().map(|position| position.group),
			Some(0)
		);

		// And a repeated cut rolls once, not once per call.
		producer.cut().unwrap();
		producer.cut().unwrap();
		producer.update(&json!({ "a": 2 })).unwrap();
		producer.finish().unwrap();

		assert_eq!(
			track.largest().now_or_never().flatten().map(|position| position.group),
			Some(1)
		);
		assert_eq!(drain(track), vec![json!({ "a": 2 })]);
	}

	#[test]
	fn a_cut_is_inert_without_deltas() {
		let (mut producer, mut track) = producer(cfg(false));
		producer.update(&json!({ "a": 1 })).unwrap();

		// With deltas off every frame already closes its own group, so there is never one to cut.
		producer.cut().unwrap();
		producer.update(&json!({ "a": 2 })).unwrap();
		producer.finish().unwrap();

		assert_eq!(
			track.largest().now_or_never().flatten().map(|position| position.group),
			Some(1)
		);
		assert_eq!(drain(track), vec![json!({ "a": 2 })]);
	}

	#[test]
	fn deltas_off_snapshot_per_group() {
		let (mut producer, mut track) = producer(cfg(false));
		producer.update(&json!({ "a": 1 })).unwrap();
		producer.update(&json!({ "a": 2 })).unwrap();
		producer.finish().unwrap();

		// Two updates => two groups, each a full snapshot. A consumer that joins after both
		// exist only sees the latest, like the existing catalog consumer.
		assert_eq!(
			track.largest().now_or_never().flatten().map(|position| position.group),
			Some(1)
		);
		assert_eq!(drain(track), vec![json!({ "a": 2 })]);
	}

	#[test]
	fn live_consumer_sees_each_update() {
		let (mut producer, track) = producer(Config::default());
		let mut consumer = Consumer::<Value>::new(track, consumer::Config::default());
		let waiter = kio::Waiter::noop();

		for n in 1..=3 {
			producer.update(&json!({ "a": n })).unwrap();
			match consumer.poll_latest(&waiter) {
				Poll::Ready(Ok(Some(value))) => assert_eq!(value.value, json!({ "a": n })),
				other => panic!("expected value, got {other:?}"),
			}
		}
	}

	/// `write_snapshot` closes the previous group and publishes a new one before writing the frame,
	/// so rejecting an oversize frame inside `write_frame` would leave an empty newest group behind.
	/// A snapshot consumer jumps to the newest, so the last good value would vanish on a failed
	/// update.
	#[test]
	fn a_rejected_update_leaves_the_previous_value_readable() {
		let (mut producer, track) = producer(cfg(false));
		producer.update(&json!({ "keep": true })).unwrap();

		// Serializes past the group cache limit, so the frame cannot be published.
		let oversized = json!({ "big": "x".repeat(moq_net::group::MAX_CACHE_BYTES as usize + 1) });
		assert!(producer.update(&oversized).is_err());
		producer.finish().unwrap();

		// A reader arriving now still finds the last good value, not an empty superseding group.
		assert_eq!(drain(track), vec![json!({ "keep": true })]);
	}

	#[test]
	fn unchanged_value_writes_nothing() {
		let (mut producer, mut track) = producer(Config::default());
		assert_eq!(producer.update(&json!({ "a": 1 })).unwrap(), Some(7));
		assert_eq!(
			producer.update(&json!({ "a": 1 })).unwrap(),
			None,
			"an unchanged value reports that nothing was written"
		);
		producer.finish().unwrap();

		assert_eq!(
			track.largest().now_or_never().flatten().map(|position| position.group),
			Some(0)
		);
		assert_eq!(drain(track), vec![json!({ "a": 1 })]);
	}

	/// A stamped value is written at its capture time, snapshot and delta alike, and the returned
	/// size is the encoded frame.
	#[test]
	fn a_stamped_update_writes_its_capture_time() {
		let (mut producer, _track) = producer_on(cfg(true), timed());
		let mut groups = producer.consume();
		let first = moq_net::Timestamp::from_millis(1_000).unwrap();
		let second = moq_net::Timestamp::from_millis(2_000).unwrap();
		let value = json!({ "a": 1, "b": "x".repeat(64) });
		let size = producer.update(moq_net::Timed::from(&value).at(first)).unwrap();
		let changed = json!({ "a": 2, "b": "x".repeat(64) });
		let delta = producer.update(moq_net::Timed::from(&changed).at(second)).unwrap();

		let waiter = kio::Waiter::noop();
		let Poll::Ready(Ok(Some(mut group))) = groups.poll_recv_group(&waiter) else {
			panic!("expected a group");
		};
		for (stamp, size) in [(first, size), (second, delta)] {
			let Poll::Ready(Ok(Some(frame))) = group.poll_read_frame(&waiter) else {
				panic!("expected a frame");
			};
			assert_eq!(frame.timestamp.unwrap().as_micros(), stamp.as_micros());
			assert_eq!(size, Some(frame.payload.len()));
		}
	}

	/// With nobody joining, deltas pile up in one group however far they outweigh the snapshot.
	#[test]
	fn deltas_never_roll_without_a_joiner() {
		let (mut producer, mut track) = producer_on(cfg(true), moq_net::track::Info::default().with_refresh(1));
		for n in 0..=10 {
			producer.update(&json!({ "n": n })).unwrap();
		}
		assert!(producer.poll_refresh(&kio::Waiter::noop()).is_pending());
		producer.finish().unwrap();

		assert_eq!(
			track.largest().now_or_never().flatten().map(|position| position.group),
			Some(0)
		);
		assert_eq!(drain(track).last().unwrap(), &json!({ "n": 10 }));
	}

	/// The frames of `group` received so far: each timestamp and decoded payload.
	fn frames(group: &mut moq_net::group::Consumer) -> Vec<(Option<moq_net::Timestamp>, Value)> {
		let waiter = kio::Waiter::noop();
		let mut out = Vec::new();
		while let Poll::Ready(Ok(Some(frame))) = group.poll_read_frame(&waiter) {
			out.push((frame.timestamp, serde_json::from_slice(&frame.payload).unwrap()));
		}
		out
	}

	/// Every group `subscriber` has received so far.
	fn groups(subscriber: &mut moq_net::track::Subscriber) -> Vec<moq_net::group::Consumer> {
		let waiter = kio::Waiter::noop();
		let mut out = Vec::new();
		while let Poll::Ready(Ok(Some(group))) = subscriber.poll_recv_group(&waiter) {
			out.push(group);
		}
		out
	}

	/// A joiner under the ratio reads the open group, and one over it gets the value restated in a
	/// fresh group, where the next change rides as a delta.
	#[test]
	fn a_joiner_over_the_ratio_gets_a_fresh_snapshot() {
		let waiter = kio::Waiter::noop();
		let (mut producer, _track) = producer_on(cfg(true), moq_net::track::Info::default().with_refresh(1));

		// A 13 byte snapshot and a 7 byte delta: under the ratio.
		producer.update(&json!({ "n": 0, "p": 0 })).unwrap();
		producer.update(&json!({ "n": 1, "p": 0 })).unwrap();
		let _early = producer.consume();
		assert!(producer.poll_refresh(&waiter).is_pending(), "a join under the ratio");

		// A second delta takes the group past it, and the next join asks for a fresh one.
		producer.update(&json!({ "n": 2, "p": 0 })).unwrap();
		assert!(producer.poll_refresh(&waiter).is_pending(), "no join, no refresh");
		let mut late = producer.consume();
		assert!(matches!(producer.poll_refresh(&waiter), Poll::Ready(Ok(()))));
		assert!(producer.poll_refresh(&waiter).is_pending(), "answered once");

		let mut fresh = groups(&mut late).pop().expect("the fresh group");
		assert_eq!(fresh.sequence, 1);
		assert_eq!(frames(&mut fresh), vec![(None, json!({ "n": 2, "p": 0 }))]);

		producer.update(&json!({ "n": 3, "p": 0 })).unwrap();
		assert_eq!(frames(&mut fresh), vec![(None, json!({ "n": 3 }))]);
	}

	/// A refresh restates the value at the newest timestamp written rather than inventing one.
	#[test]
	fn a_refresh_reuses_the_newest_timestamp() {
		let ms = |n| moq_net::Timestamp::from_millis(n).unwrap();
		let (mut producer, _track) = producer_on(cfg(true), timed().with_refresh(1));
		producer
			.update(moq_net::Timed::from(&json!({ "n": 0 })).at(ms(100)))
			.unwrap();
		for n in 1..=3 {
			producer
				.update(moq_net::Timed::from(&json!({ "n": n })).at(ms(100 + n)))
				.unwrap();
		}

		let mut late = producer.consume();
		assert!(matches!(
			producer.poll_refresh(&kio::Waiter::noop()),
			Poll::Ready(Ok(()))
		));
		let mut fresh = groups(&mut late).pop().expect("the fresh group");
		assert_eq!(fresh.sequence, 1);
		assert_eq!(frames(&mut fresh), vec![(Some(ms(103)), json!({ "n": 3 }))]);
	}

	/// A delta may carry a timestamp older than its group's first frame, but a new group may not
	/// start before that frame, so the restatement takes the newest timestamp written.
	#[test]
	fn a_refresh_after_an_older_update_starts_at_the_newest_timestamp() {
		let ms = |n| moq_net::Timestamp::from_millis(n).unwrap();
		let (mut producer, _track) = producer_on(cfg(true), timed().with_refresh(1));
		for (n, at) in [130, 110, 120, 115].into_iter().enumerate() {
			producer
				.update(moq_net::Timed::from(&json!({ "n": n })).at(ms(at)))
				.unwrap();
		}

		let mut late = producer.consume();
		assert!(matches!(
			producer.poll_refresh(&kio::Waiter::noop()),
			Poll::Ready(Ok(()))
		));
		let mut fresh = groups(&mut late).pop().expect("the fresh group");
		assert_eq!(fresh.sequence, 1);
		assert_eq!(frames(&mut fresh), vec![(Some(ms(130)), json!({ "n": 3 }))]);
	}

	/// A refresh restates the last value written, never one whose update was rejected, and the next
	/// update rides as a delta against it.
	#[test]
	fn a_refresh_after_a_rejected_update_restates_the_last_written_value() {
		let ms = |n| moq_net::Timestamp::from_millis(n).unwrap();
		let (mut producer, track) = producer_on(cfg(true), timed().with_refresh(1));
		for n in 0..=3 {
			producer
				.update(moq_net::Timed::from(&json!({ "n": n })).at(ms(100 + n)))
				.unwrap();
		}

		// Untimed on a timed track, so the frame is rejected before it reaches a group.
		assert!(matches!(
			producer.update(&json!({ "n": 99 })),
			Err(crate::Error::Net(moq_net::Error::TimestampMismatch))
		));

		let mut late = producer.consume();
		assert!(matches!(
			producer.poll_refresh(&kio::Waiter::noop()),
			Poll::Ready(Ok(()))
		));
		let mut fresh = groups(&mut late).pop().expect("the fresh group");
		assert_eq!(fresh.sequence, 1);
		assert_eq!(frames(&mut fresh), vec![(Some(ms(103)), json!({ "n": 3 }))]);

		producer
			.update(moq_net::Timed::from(&json!({ "n": 4 })).at(ms(104)))
			.unwrap();
		producer.finish().unwrap();
		assert_eq!(drain(track).last(), Some(&json!({ "n": 4 })));
	}

	/// Without a ratio on the track, no join asks for a group, however large the deltas grow.
	#[test]
	fn no_ratio_never_refreshes() {
		let (mut producer, _track) = producer(cfg(true));
		for n in 0..=10 {
			producer.update(&json!({ "n": n })).unwrap();
		}
		let _late = producer.consume();
		assert!(producer.poll_refresh(&kio::Waiter::noop()).is_pending());
	}

	/// `refresh` resolves on a join over the ratio and errors once the track closes.
	#[test]
	fn refresh_resolves_on_a_join_and_errors_on_close() {
		let (mut producer, _track) = producer_on(cfg(true), moq_net::track::Info::default().with_refresh(1));
		producer.update(&json!({ "n": 0 })).unwrap();
		producer.update(&json!({ "n": 1 })).unwrap();
		producer.update(&json!({ "n": 2 })).unwrap();

		let mut refresh = Box::pin(producer.refresh());
		assert!(refresh.as_mut().now_or_never().is_none());
		let _late = producer.consume();
		assert!(matches!(refresh.now_or_never(), Some(Ok(()))));

		producer.finish().unwrap();
		assert!(matches!(producer.refresh().now_or_never(), Some(Err(_))));
	}

	#[test]
	fn array_change_is_delta() {
		let (mut producer, mut track) = producer(cfg(true));
		producer.update(&json!({ "list": [1, 2] })).unwrap();
		producer.update(&json!({ "list": [1, 2, 3] })).unwrap();
		producer.finish().unwrap();

		// The array is replaced wholesale in a delta, so it stays in the same group.
		assert_eq!(
			track.largest().now_or_never().flatten().map(|position| position.group),
			Some(0)
		);
		assert_eq!(drain(track).last().unwrap(), &json!({ "list": [1, 2, 3] }));
	}

	#[test]
	fn frame_cap_rolls_snapshot() {
		let (mut producer, mut track) = producer(cfg(true));
		// First update is the snapshot (frame 0); then MAX_DELTA_FRAMES - 1 deltas fill the group.
		for i in 0..=MAX_DELTA_FRAMES {
			producer.update(&json!({ "n": i })).unwrap();
		}
		producer.finish().unwrap();

		// The frame cap forced exactly one extra snapshot group despite the huge ratio.
		assert_eq!(
			track.largest().now_or_never().flatten().map(|position| position.group),
			Some(1)
		);
		assert_eq!(drain(track).last().unwrap(), &json!({ "n": MAX_DELTA_FRAMES }));
	}

	#[test]
	fn modify_refuses_a_value_of_another_shape() {
		// A published value that does not deserialize as `T` fails the edit instead of seeding a
		// default and publishing a value with every other field dropped.
		#[derive(serde::Deserialize, Default, PartialEq, Debug)]
		struct Doc {
			count: u32,
		}

		// Serializes to a shape its own `Deserialize` refuses, the way a schema drift does.
		impl serde::Serialize for Doc {
			fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
				json!({ "count": self.count.to_string(), "other": 1 }).serialize(serializer)
			}
		}

		let track = moq_net::broadcast::Info::new()
			.produce()
			.create_track("test", moq_net::track::Info::default())
			.unwrap();
		let consumer = track.subscribe(None);
		let mut producer = Producer::<Doc>::new(track, Config::default());
		producer.update(&Doc { count: 1 }).unwrap();

		assert!(matches!(producer.modify(None), Err(crate::Error::Json(_))));

		// The refused edit published nothing after the original value.
		producer.finish().unwrap();
		assert_eq!(drain(consumer), vec![json!({ "count": "1", "other": 1 })]);
	}

	/// An end declared ahead of the live edge still admits the edit below it, and refuses the one
	/// that would cross it.
	#[test]
	fn modify_admits_an_edit_below_a_declared_end() {
		let mut track = moq_net::broadcast::Info::new()
			.produce()
			.create_track("test", moq_net::track::Info::default())
			.unwrap();
		let mut producer = Producer::<Value>::new(track.clone(), cfg(false));
		producer.update(&json!({ "a": 1 })).unwrap();
		track.finish_at(moq_net::track::Position::group(2)).unwrap();

		producer.modify(None).unwrap()["a"] = json!(2);
		assert!(matches!(
			producer.modify(None),
			Err(crate::Error::Net(moq_net::Error::Closed))
		));
	}

	#[test]
	fn modify_composes_independent_owners() {
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
			.create_track("test", moq_net::track::Info::default())
			.unwrap();
		let consumer = track.subscribe(None);
		let mut producer = Producer::<Doc>::new(track, Config::default());

		// First owner sets its field.
		producer.modify(None).unwrap().video = Some("v1".to_string());

		// Second owner starts from the latest value and adds its own field without clobbering.
		producer.modify(None).unwrap().scte35 = Some(42);

		// Locking without mutating publishes nothing (the guard stays clean).
		let _ = producer.modify(None).unwrap();

		producer.finish().unwrap();

		let mut consumer = Consumer::<Doc>::new(consumer, consumer::Config::default());
		let waiter = kio::Waiter::noop();
		let mut last = None;
		while let Poll::Ready(Ok(Some(value))) = consumer.poll_latest(&waiter) {
			last = Some(value.value);
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
	fn mutate_composes_independent_owners() {
		// The closure form of the guard: the JS `Producer.mutate` rules, in Rust.
		#[derive(serde::Serialize, serde::Deserialize, Default, PartialEq, Debug)]
		struct Doc {
			#[serde(skip_serializing_if = "Option::is_none")]
			video: Option<String>,
			#[serde(skip_serializing_if = "Option::is_none")]
			scte35: Option<u32>,
		}

		let track = moq_net::broadcast::Info::new()
			.produce()
			.create_track("test", moq_net::track::Info::default())
			.unwrap();
		let mut consumer = track.subscribe(None);
		let mut producer = Producer::<Doc>::new(track, cfg(false));

		producer.mutate(|doc| doc.video = Some("v1".to_string()), None).unwrap();

		// The second owner starts from the latest value and adds its own field without clobbering.
		producer.mutate(|doc| doc.scte35 = Some(42), None).unwrap();

		// A closure that changes nothing publishes nothing: deltas are off, so each publish would
		// otherwise open a group of its own.
		producer.mutate(|_| {}, None).unwrap();
		assert_eq!(
			consumer
				.largest()
				.now_or_never()
				.flatten()
				.map(|position| position.group),
			Some(1)
		);

		producer.finish().unwrap();

		let mut consumer = Consumer::<Doc>::new(consumer, consumer::Config::default());
		let waiter = kio::Waiter::noop();
		let mut last = None;
		while let Poll::Ready(Ok(Some(value))) = consumer.poll_latest(&waiter) {
			last = Some(value.value);
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
	fn mutate_returns_the_publish_error() {
		// Unlike a dropped guard, the closure form hands the failure back to the caller.
		let (mut producer, _track) = producer(cfg(false));
		producer.update(&json!({ "keep": true })).unwrap();

		assert!(matches!(
			producer.mutate(
				|value| {
					*value = json!({ "big": "x".repeat(moq_net::group::MAX_CACHE_BYTES as usize + 1) });
				},
				None
			),
			Err(crate::Error::Net(moq_net::Error::FrameTooLarge))
		));
	}

	#[test]
	fn a_dropped_edit_publishes_a_snapshot_then_a_delta() {
		let (mut producer, mut track) = producer(cfg(true));

		*producer.modify(None).unwrap() = json!({ "a": 1, "b": 1 });
		*producer.modify(None).unwrap() = json!({ "a": 1, "b": 2 });
		producer.finish().unwrap();

		// The ratio keeps both edits in one group: a snapshot plus a merge-patch delta.
		assert_eq!(
			track.largest().now_or_never().flatten().map(|position| position.group),
			Some(0)
		);
		assert_eq!(drain(track).last().unwrap(), &json!({ "a": 1, "b": 2 }));
	}

	#[test]
	fn modify_refuses_a_finished_track() {
		let (mut producer, track) = producer(cfg(false));
		producer.update(&json!({ "a": 1 })).unwrap();
		producer.finish().unwrap();

		assert!(matches!(
			producer.modify(None),
			Err(crate::Error::Net(moq_net::Error::Closed))
		));
		assert_eq!(drain(track), vec![json!({ "a": 1 })]);
	}

	#[test]
	fn a_dropped_failed_edit_aborts_the_track() {
		let (mut producer, track) = producer(cfg(false));
		producer.update(&json!({ "keep": true })).unwrap();
		let mut subscriber = track.ordered();

		*producer.modify(None).unwrap() = json!({ "big": "x".repeat(moq_net::group::MAX_CACHE_BYTES as usize + 1) });

		// The publisher learns the cause at its next edit. The consumer drains the snapshot
		// that finished, then learns it from the aborted track.
		assert!(matches!(
			producer.modify(None),
			Err(crate::Error::Net(moq_net::Error::FrameTooLarge))
		));
		assert!(matches!(
			subscriber.poll_next_group(&kio::Waiter::noop()),
			Poll::Ready(Ok(Some(_)))
		));
		assert!(matches!(
			subscriber.poll_next_group(&kio::Waiter::noop()),
			Poll::Ready(Err(moq_net::Error::FrameTooLarge))
		));
	}

	#[test]
	fn a_failed_commit_leaves_the_track_open() {
		let (mut producer, track) = producer(cfg(false));
		producer.update(&json!({ "keep": true })).unwrap();

		let mut guard = producer.modify(None).unwrap();
		*guard = json!({ "big": "x".repeat(moq_net::group::MAX_CACHE_BYTES as usize + 1) });
		assert!(matches!(
			guard.commit(),
			Err(crate::Error::Net(moq_net::Error::FrameTooLarge))
		));

		// The caller got the error, so the track stays usable for a smaller value.
		*producer.modify(None).unwrap() = json!({ "keep": false });
		producer.finish().unwrap();
		assert_eq!(drain(track).last().unwrap(), &json!({ "keep": false }));
	}

	#[test]
	fn a_panicking_edit_does_not_publish() {
		let (mut producer, mut track) = producer(cfg(false));
		producer.update(&json!({ "a": 1 })).unwrap();

		let panicked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
			let mut guard = producer.modify(None).unwrap();
			*guard = json!({ "a": 2 });
			panic!("torn edit");
		}));
		assert!(panicked.is_err());

		// A discarded edit is not a publish failure, so the track is still open for the next one.
		*producer.modify(None).unwrap() = json!({ "a": 3 });
		producer.finish().unwrap();
		assert_eq!(
			track.largest().now_or_never().flatten().map(|position| position.group),
			Some(1),
			"the torn edit took a group"
		);
		assert_eq!(drain(track).last().unwrap(), &json!({ "a": 3 }));
	}

	/// A rejected edit never reached the wire, so the next `modify` starts from the last value that
	/// did: neither erased (every other field dropped) nor carrying the rejected edit, which would
	/// fail again, or publish something its owner was told failed, under whoever edits next.
	#[test]
	fn a_rejected_edit_leaves_modify_at_the_last_written_value() {
		let ms = |n| moq_net::Timestamp::from_millis(n).unwrap();
		let (mut producer, _track) = producer_on(cfg(true), timed());
		producer
			.mutate(|value| *value = json!({ "video": "v1" }), Some(ms(100)))
			.unwrap();

		// Untimed on a timed track, so the frame is rejected before it reaches a group.
		assert!(matches!(
			producer.mutate(|value| value["scte35"] = json!(42), None),
			Err(crate::Error::Net(moq_net::Error::TimestampMismatch))
		));

		let guard = producer.modify(Some(ms(101))).unwrap();
		assert_eq!(*guard, json!({ "video": "v1" }));
	}

	/// Finishing takes the open group with it, so the encoder must stop emitting deltas into a group
	/// that no longer exists. A later update has to surface the closed track as an error rather than
	/// writing a delta with nowhere to put it.
	#[test]
	fn update_after_finish_errors_instead_of_panicking() {
		let (mut producer, _track) = producer(cfg(true));
		producer.update(&json!({ "a": 1 })).unwrap();
		producer.update(&json!({ "a": 2 })).unwrap(); // a delta, so a group is open
		producer.finish().unwrap();

		assert!(matches!(producer.update(&json!({ "a": 3 })), Err(crate::Error::Net(_))));
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

	/// A time [`rejecting_track`] can't hold: it matches the timed track, so the publish gets as
	/// far as the group before the conversion fails.
	fn late() -> moq_net::Timestamp {
		moq_net::Timestamp::from_millis(10_000).unwrap()
	}

	/// `append_group` publishes the group before the frame is written, and a rejected frame does not
	/// close the track. Dropping the handle does not close the group either, so without an explicit
	/// close a subscriber that advanced into it waits there forever.
	#[test]
	fn a_rejected_snapshot_does_not_strand_an_empty_group() {
		let track = rejecting_track();
		let mut subscriber = track.subscribe(None).ordered();
		let mut producer = Producer::<Value>::new(track, cfg(false));

		assert!(matches!(
			producer.update(moq_net::Timed::from(&json!({ "a": 1 })).at(late())),
			Err(crate::Error::Net(_))
		));

		let waiter = kio::Waiter::noop();
		let Poll::Ready(Ok(Some(mut group))) = subscriber.poll_next_group(&waiter) else {
			panic!("the group was published, so a subscriber sees it");
		};
		assert!(
			matches!(group.poll_read_frame(&waiter), Poll::Ready(Ok(None))),
			"the empty group must be closed, not left open for a subscriber to wait in"
		);
	}

	#[test]
	fn commit_publishes_once() {
		#[derive(serde::Serialize, serde::Deserialize, Default, PartialEq, Debug)]
		struct Doc {
			a: u32,
		}

		let track = moq_net::broadcast::Info::new()
			.produce()
			.create_track("test", moq_net::track::Info::default())
			.unwrap();
		let mut consumer = track.subscribe(None);
		let mut producer = Producer::<Doc>::new(track, cfg(false));

		let mut guard = producer.modify(None).unwrap();
		guard.a = 1;
		guard.commit().unwrap();

		// The drop that follows `commit` must not publish a second group.
		producer.finish().unwrap();
		assert_eq!(
			consumer
				.largest()
				.now_or_never()
				.flatten()
				.map(|position| position.group),
			Some(0)
		);
	}

	#[test]
	fn newer_group_supersedes_in_progress_reconstruction() {
		// A couple of deltas fill group 0, then a cut leaves the last update to open group 1.
		let (mut producer, track) = producer(cfg(true));
		let mut observer = producer.consume();
		let mut consumer = Consumer::<Value>::new(track, consumer::Config::default());
		let waiter = kio::Waiter::noop();

		producer.update(&json!({ "a": 1 })).unwrap(); // snapshot, group 0
		match consumer.poll_latest(&waiter) {
			Poll::Ready(Ok(Some(value))) => assert_eq!(value.value, json!({ "a": 1 })),
			other => panic!("expected first value, got {other:?}"),
		}

		producer.update(&json!({ "a": 2 })).unwrap(); // delta in group 0
		producer.update(&json!({ "a": 3 })).unwrap(); // delta in group 0
		producer.cut().unwrap();
		producer.update(&json!({ "a": 4 })).unwrap(); // snapshot, group 1
		producer.finish().unwrap();
		assert_eq!(
			observer
				.largest()
				.now_or_never()
				.flatten()
				.map(|position| position.group),
			Some(1)
		);

		// The consumer jumps to the newest group and never yields a stale value.
		let mut last = None;
		while let Poll::Ready(Ok(Some(value))) = consumer.poll_latest(&waiter) {
			last = Some(value.value);
		}
		assert_eq!(last.unwrap(), json!({ "a": 4 }));
	}

	#[test]
	fn open_group_pends_after_track_finish() {
		// A group appended before the track finishes may still deliver frames, so the consumer must
		// keep waiting on it rather than ending the stream. Regression for the backlog-collapse poll.
		let track = moq_net::broadcast::Info::new()
			.produce()
			.create_track("test", timed())
			.unwrap();
		let mut group = track.append_group().unwrap();
		let consumer_track = track.subscribe(None);
		track.finish().unwrap();

		let mut consumer = Consumer::<Value>::new(consumer_track, consumer::Config::default());
		let waiter = kio::Waiter::noop();

		// Track is finished but the open group is empty: pending, not end-of-stream.
		assert!(matches!(consumer.poll_latest(&waiter), Poll::Pending));

		group
			.write_frame(
				moq_net::Timestamp::ZERO,
				Bytes::from(serde_json::to_vec(&json!({ "a": 1 })).unwrap()),
			)
			.unwrap();
		group.finish().unwrap();

		match consumer.poll_latest(&waiter) {
			Poll::Ready(Ok(Some(value))) => assert_eq!(value.value, json!({ "a": 1 })),
			other => panic!("expected the catalog value, got {other:?}"),
		}
	}

	#[test]
	fn late_joiner_collapses_backlog_to_latest() {
		// A whole group's worth of snapshot + deltas is buffered before the consumer reads. It should
		// apply them all but yield only the latest value once, not replay every superseded state.
		let (mut producer, mut track) = producer(cfg(true));
		for n in 0..=20 {
			producer.update(&json!({ "n": n })).unwrap();
		}
		producer.finish().unwrap();

		// One group (ratio is generous), so a single poll drains the backlog into one yield.
		assert_eq!(
			track.largest().now_or_never().flatten().map(|position| position.group),
			Some(0)
		);
		let values = drain(track);
		assert_eq!(
			values,
			vec![json!({ "n": 20 })],
			"backlog should collapse to the latest value"
		);
	}

	#[test]
	fn compressed_late_joiner_collapses_backlog_to_latest() {
		// Same collapse, read through moq-net's inflated copy of the group.
		let (mut producer, mut track) = producer_on(cfg(true), compressed());
		for n in 0..=20 {
			producer.update(&json!({ "n": n })).unwrap();
		}
		producer.finish().unwrap();

		assert_eq!(
			track.largest().now_or_never().flatten().map(|position| position.group),
			Some(0)
		);
		let values = drain(track);
		assert_eq!(
			values,
			vec![json!({ "n": 20 })],
			"compressed backlog should collapse to the latest"
		);
	}

	#[test]
	fn compressed_snapshot_per_group_roundtrips() {
		let (mut producer, mut track) = producer_on(cfg(false), compressed());
		producer.update(&json!({ "a": 1 })).unwrap();
		producer.update(&json!({ "a": 2 })).unwrap();
		producer.finish().unwrap();

		// Deltas disabled: one compressed snapshot per group, latest reconstructs identically.
		assert_eq!(
			track.largest().now_or_never().flatten().map(|position| position.group),
			Some(1)
		);
		let values = drain(track);
		assert_eq!(values, vec![json!({ "a": 2 })]);
	}

	/// A refresh on a compressed track opens a fresh group, so a late joiner rebuilds the value from
	/// the restated group alone.
	#[test]
	fn a_compressed_refresh_opens_a_fresh_group() {
		let (mut producer, mut track) = producer_on(cfg(true), compressed().with_refresh(1));
		for n in 0..=40 {
			producer.update(&json!({ "n": n })).unwrap();
		}
		let late = producer.consume();
		assert!(matches!(
			producer.poll_refresh(&kio::Waiter::noop()),
			Poll::Ready(Ok(()))
		));
		producer.update(&json!({ "n": 41 })).unwrap();
		producer.finish().unwrap();

		assert_eq!(
			track.largest().now_or_never().flatten().map(|position| position.group),
			Some(1)
		);
		assert_eq!(drain(late).last().unwrap(), &json!({ "n": 41 }));
	}

	#[test]
	fn rejected_field_names_its_path() {
		#[derive(serde::Deserialize)]
		#[allow(dead_code, reason = "the test only checks that deserializing fails")]
		struct Inner {
			count: u8,
		}
		#[derive(serde::Deserialize)]
		#[allow(dead_code, reason = "the test only checks that deserializing fails")]
		struct Outer {
			inner: Inner,
		}

		let (mut producer, track) = producer(cfg(false));
		producer.update(&json!({ "inner": { "count": 300 } })).unwrap();

		let mut consumer = Consumer::<Outer>::new(track, consumer::Config::default());
		let Poll::Ready(Err(err)) = consumer.poll_next(&kio::Waiter::noop()) else {
			panic!("expected a deserialize error");
		};

		// Deserializing from a Value has no line/column, so the path is the only locator a
		// consumer gets. See https://github.com/moq-dev/moq/issues/2509.
		assert!(err.to_string().starts_with("json: inner.count: "), "{err}");
	}

	#[test]
	fn rejected_root_omits_the_path() {
		let (mut producer, track) = producer(cfg(false));
		producer.update(&json!("not a map")).unwrap();

		let mut consumer = Consumer::<std::collections::BTreeMap<String, u8>>::new(track, consumer::Config::default());
		let Poll::Ready(Err(err)) = consumer.poll_next(&kio::Waiter::noop()) else {
			panic!("expected a deserialize error");
		};
		assert_eq!(
			err.to_string(),
			"json: invalid type: string \"not a map\", expected a map"
		);
	}
}
