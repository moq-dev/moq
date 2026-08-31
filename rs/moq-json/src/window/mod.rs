//! Sliding-window JSON publishing over [`moq-net`](moq_net) tracks.
//!
//! A window is an ordered run of records the publisher appends to the back of and drops from the
//! front of. Unlike [`stream`](crate::stream), which preserves a log forever in one group, and
//! [`snapshot`](crate::snapshot), which keeps only the latest value, a window keeps a bounded
//! stretch of records and lets a reader join it at any point.
//!
//! # Why a log can't do this
//!
//! The obvious alternative is an append-only log that rolls its group and re-seeds the new one with
//! the records it still holds. That breaks the reader: re-seeded records are indistinguishable from
//! new ones, so a reader that was keeping up receives them twice. This mode exists to make the
//! restatement explicit, so a reader can tell "you already have these" from "here is another one".
//!
//! # On the wire
//!
//! The first frame of every group names the retained `offset` and a decodable `records` suffix. Its
//! optional `start` identifies the suffix when a checkpoint bound omits older records. Later frames
//! are tagged `push` and `pop` ops, positional against the group header.
//! Indices stop at 2^53 - 1, the largest integer represented exactly by both implementations.
//!
//! Trimming is therefore an op, not a group boundary. Dropping a record costs one small frame
//! inside the shared compression window instead of a roll that would throw that window away.
//!
//! # Group boundaries are invisible
//!
//! The publisher rolls a group when the ops in it outgrow
//! [`ProducerConfig::op_ratio`](ProducerConfig::op_ratio) times the header that opened it, exactly as
//! [`snapshot`](crate::snapshot) rolls on its delta budget. That is purely a compression decision:
//! there is no caller-driven cut and no age bound, and a [`Consumer`] never surfaces it. A header
//! restating records a reader already has yields nothing, so however often the publisher rolls, the
//! reader sees one continuous stream of [`Event`]s. [`ProducerConfig::checkpoint_records`] bounds
//! the suffix repeated on each roll for a long-lived window.
//!
//! # What a reader is told
//!
//! A reader gets [`Event::Push`] when a record arrives, [`Event::Pop`] when a contiguous range
//! leaves, and [`Event::Skip`] when a range was dropped before this reader saw it. A reader that
//! keeps up sees pushes and pops; one that falls a group behind learns from the header's offset which
//! records it will never get, rather than silently missing them.
//!
//! # Choosing a layer
//!
//! [`Producer`] and [`Consumer`] own a track. [`Encoder`] and [`Decoder`] are the same logic
//! without it, for when something else is already in charge of the track; the encoder owns the
//! retained window and says where the group boundaries fall, and the decoder turns frames into
//! events.

mod consumer;
mod decoder;
mod encoder;
mod op;
mod producer;

pub use consumer::Consumer;
pub use decoder::{ConsumerConfig, Decoder, Event, Group};
pub use encoder::{Encoded, Encoder, Pending, ProducerConfig};
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

	#[test]
	#[should_panic(expected = "checkpoint_records must be positive")]
	fn zero_checkpoint_records_is_rejected() {
		let config = ProducerConfig {
			checkpoint_records: Some(0),
			..Default::default()
		};
		let _ = Encoder::<Value>::new(config);
	}

	fn consumer(track: moq_net::track::Subscriber, compression: bool) -> Consumer<Value> {
		Consumer::new(track, ConsumerConfig::default().with_compression(compression))
	}

	/// A track whose timestamp conversion rejects every frame after its group is published.
	fn rejecting_track() -> moq_net::track::Producer {
		let mut info = moq_net::track::Info::default();
		info.timescale = moq_net::Timescale::new((1u64 << 62) - 1).unwrap();

		moq_net::broadcast::Info::new()
			.produce()
			.create_track("test", Some(info))
			.unwrap()
	}

	/// Drain every event currently available without blocking.
	fn drain(consumer: &mut Consumer<Value>) -> Vec<Event<Value>> {
		let waiter = kio::Waiter::noop();
		let mut out = Vec::new();
		while let Poll::Ready(Ok(Some(event))) = consumer.poll_next(&waiter) {
			out.push(event);
		}
		out
	}

	fn rec(n: u64) -> Value {
		json!({ "n": n })
	}

	/// A producer and a consumer that reads after every edit.
	///
	/// Polling as the publisher goes is what "keeping up" means: a consumer left until the end is a
	/// whole group behind, and the default subscription abandons a group as soon as a newer one
	/// exists, so it would resume at the newest header instead of reading the rolls in between.
	struct Live {
		producer: Producer<Value>,
		consumer: Consumer<Value>,
		events: Vec<Event<Value>>,
	}

	impl Live {
		fn new(config: ProducerConfig) -> Self {
			let compression = config.compression;
			let (producer, track) = producer(config);
			Self {
				producer,
				consumer: consumer(track, compression),
				events: Vec::new(),
			}
		}

		fn push(&mut self, n: u64) {
			self.producer.push(&rec(n)).unwrap();
			self.read();
		}

		fn pop(&mut self, count: u64) {
			self.producer.pop(count).unwrap();
			self.read();
		}

		fn read(&mut self) {
			self.events.extend(drain(&mut self.consumer));
		}

		fn finish(self) -> Vec<Event<Value>> {
			let Live {
				producer,
				mut consumer,
				mut events,
			} = self;
			producer.finish().unwrap();
			events.extend(drain(&mut consumer));
			events
		}

		/// Just the indices pushed, in order.
		fn pushed(events: &[Event<Value>]) -> Vec<u64> {
			events
				.iter()
				.filter_map(|e| match e {
					Event::Push { index, .. } => Some(*index),
					_ => None,
				})
				.collect()
		}
	}

	#[test]
	fn push_and_pop_round_trip() {
		let mut live = Live::new(ProducerConfig::default());
		live.push(0);
		live.push(1);
		live.pop(1);
		live.push(2);

		assert_eq!(
			live.finish(),
			vec![
				Event::Push {
					index: 0,
					value: rec(0)
				},
				Event::Push {
					index: 1,
					value: rec(1)
				},
				Event::Pop(0..1),
				Event::Push {
					index: 2,
					value: rec(2)
				},
			]
		);
	}

	#[test]
	fn the_window_slides() {
		let (mut producer, _track) = producer(ProducerConfig::default());
		for n in 0..5 {
			producer.push(&rec(n)).unwrap();
			if n >= 2 {
				producer.pop(1).unwrap();
			}
		}

		// Three pops leave the two newest records, at indices 3 and 4.
		assert_eq!(producer.range(), 3..5);
		assert_eq!(producer.window(), vec![rec(3), rec(4)]);
	}

	#[test]
	fn a_popped_record_is_never_restated() {
		// Ops disabled, so every single edit is its own group restating the whole window.
		let mut live = Live::new(ProducerConfig::default().with_op_ratio(0));
		live.push(0);
		live.push(1);
		live.pop(1);
		live.push(2);

		// Every edit restates the window, yet a record already delivered is never pushed twice. That
		// is the property an append-only log cannot provide.
		assert_eq!(
			live.finish(),
			vec![
				Event::Push {
					index: 0,
					value: rec(0)
				},
				Event::Push {
					index: 1,
					value: rec(1)
				},
				Event::Pop(0..1),
				Event::Push {
					index: 2,
					value: rec(2)
				},
			]
		);
	}

	#[test]
	fn bounded_checkpoints_keep_a_following_consumer_contiguous() {
		let config = ProducerConfig::default().with_op_ratio(0).with_checkpoint_records(2);
		let mut live = Live::new(config);
		for n in 0..6 {
			live.push(n);
		}

		assert_eq!(live.producer.range(), 0..6);
		assert_eq!(live.producer.window(), vec![rec(4), rec(5)]);
		let events = live.finish();
		assert_eq!(Live::pushed(&events), (0..6).collect::<Vec<_>>());
		assert!(!events.iter().any(|event| matches!(event, Event::Skip(_))));
	}

	#[test]
	fn a_late_consumer_skips_to_the_bounded_checkpoint() {
		let config = ProducerConfig::default().with_op_ratio(0).with_checkpoint_records(2);
		let mut encoder = Encoder::<Value>::new(config);
		let mut latest = None;
		for n in 0..5 {
			let frame = encoder.push(&rec(n)).unwrap();
			latest = Some(frame.payload.clone());
			frame.commit();
		}

		let mut decoder = Decoder::<Value>::new(ConsumerConfig::default());
		decoder.group().decode(&latest.unwrap()).unwrap();
		assert_eq!(
			std::iter::from_fn(|| decoder.next_event()).collect::<Vec<_>>(),
			vec![
				Event::Skip(0..3),
				Event::Push {
					index: 3,
					value: rec(3)
				},
				Event::Push {
					index: 4,
					value: rec(4)
				},
			]
		);
	}

	#[test]
	fn pops_cross_the_omitted_checkpoint_prefix() {
		let config = ProducerConfig::default().with_checkpoint_records(2);
		let mut encoder = Encoder::<Value>::new(config);
		for n in 0..5 {
			encoder.push(&rec(n)).unwrap().commit();
		}
		assert_eq!(encoder.range(), 0..5);
		assert_eq!(encoder.window(), vec![rec(3), rec(4)]);

		encoder.pop(2).unwrap().unwrap().commit();
		assert_eq!(encoder.range(), 2..5);
		assert_eq!(encoder.window(), vec![rec(3), rec(4)]);

		encoder.pop(2).unwrap().unwrap().commit();
		assert_eq!(encoder.range(), 4..5);
		assert_eq!(encoder.window(), vec![rec(4)]);
	}

	#[test]
	fn a_fresh_consumer_adopts_the_offset_without_skipping_history() {
		let track = moq_net::broadcast::Info::new()
			.produce()
			.create_track("test", None)
			.unwrap();
		let mut producer = Producer::<Value>::new(track, ProducerConfig::default().with_op_ratio(0));

		for n in 0..5 {
			producer.push(&rec(n)).unwrap();
		}
		producer.pop(3).unwrap();
		let mut subscriber = producer.consume();
		subscriber.start_at(subscriber.latest().unwrap());
		let mut fresh = consumer(subscriber, false);
		producer.finish().unwrap();

		// Joining at offset 3 must not report 3 skips for records that were never this reader's to
		// miss: it simply starts where the window starts.
		let events = drain(&mut fresh);
		assert_eq!(
			events,
			vec![
				Event::Push {
					index: 3,
					value: rec(3)
				},
				Event::Push {
					index: 4,
					value: rec(4)
				}
			]
		);
		assert!(!events.iter().any(|e| matches!(e, Event::Skip(_))));
	}

	#[test]
	fn a_lagging_consumer_is_told_what_it_missed() {
		// Ops are disabled, so every edit opens a new group. Feed the first two groups to the
		// decoder, skip the middle groups as a lagging track subscriber would, then resume at the
		// latest header.
		let mut encoder = Encoder::<Value>::new(ProducerConfig::default().with_op_ratio(0));
		let mut decoder = Decoder::<Value>::new(ConsumerConfig::default());
		for n in 0..2 {
			let frame = encoder.push(&rec(n)).unwrap();
			let mut group = decoder.group();
			group.decode(&frame.payload).unwrap();
			frame.commit();
		}
		assert_eq!(
			std::iter::from_fn(|| decoder.next_event()).collect::<Vec<_>>(),
			vec![
				Event::Push {
					index: 0,
					value: rec(0)
				},
				Event::Push {
					index: 1,
					value: rec(1)
				}
			]
		);

		let mut latest = None;
		for n in 2..8 {
			let frame = encoder.push(&rec(n)).unwrap();
			frame.commit();

			let frame = encoder.pop(1).unwrap().unwrap();
			latest = Some(frame.payload.clone());
			frame.commit();
		}
		let mut group = decoder.group();
		group.decode(&latest.unwrap()).unwrap();

		let events = std::iter::from_fn(|| decoder.next_event()).collect::<Vec<_>>();
		let skipped: Vec<std::ops::Range<u64>> = events
			.iter()
			.filter_map(|e| match e {
				Event::Skip(range) => Some(range.clone()),
				_ => None,
			})
			.collect();

		// Records 2..=5 existed but this reader will never receive them, and it is told so rather than
		// silently jumping from 1 to 6.
		assert!(!skipped.is_empty(), "expected skips, got {events:?}");
		assert_eq!(skipped.first().map(|range| range.start), Some(2));

		// Every index is still accounted for exactly once, in order.
		let reported: Vec<u64> = events
			.iter()
			.flat_map(|e| match e {
				Event::Push { index, .. } => vec![*index],
				Event::Skip(range) => range.clone().collect(),
				Event::Pop(_) => Vec::new(),
			})
			.collect();
		assert!(reported.windows(2).all(|w| w[1] == w[0] + 1), "gaps in {reported:?}");
	}

	#[test]
	fn consumer_resumes_at_a_checkpoint_after_losing_a_group() {
		for err in [moq_net::Error::Old, moq_net::Error::Lagged, moq_net::Error::Evicted] {
			let mut track = moq_net::broadcast::Info::new()
				.produce()
				.create_track("test", None)
				.unwrap();
			let mut consumer = consumer(track.subscribe(None), false);
			let mut encoder = Encoder::<Value>::new(ProducerConfig::default().with_op_ratio(0));

			let first = encoder.push(&rec(0)).unwrap();
			let payload = first.payload.clone();
			first.commit();
			let mut lost = track.append_group().unwrap();
			lost.write_frame(moq_net::Timestamp::ZERO, payload).unwrap();
			assert_eq!(
				drain(&mut consumer),
				vec![Event::Push {
					index: 0,
					value: rec(0)
				}]
			);
			lost.abort(err).unwrap();

			let second = encoder.push(&rec(1)).unwrap();
			let payload = second.payload.clone();
			second.commit();
			let mut checkpoint = track.append_group().unwrap();
			checkpoint.write_frame(moq_net::Timestamp::ZERO, payload).unwrap();
			checkpoint.finish().unwrap();
			track.finish().unwrap();

			assert_eq!(
				drain(&mut consumer),
				vec![Event::Push {
					index: 1,
					value: rec(1)
				}]
			);
		}
	}

	#[test]
	fn compressed_round_trip_across_rolls() {
		let mut live = Live::new(ProducerConfig::default().with_compression(true).with_op_ratio(1));
		for n in 0..40 {
			live.push(n);
			if n >= 10 {
				live.pop(1);
			}
		}

		// A tight ratio rolls many times; every record still arrives exactly once, in order.
		assert_eq!(Live::pushed(&live.finish()), (0..40).collect::<Vec<_>>());
	}

	#[test]
	fn an_empty_pop_writes_nothing() {
		let (mut producer, track) = producer(ProducerConfig::default());
		producer.pop(5).unwrap();
		producer.finish().unwrap();

		// Nothing was ever pushed, so there is nothing to drop and no group to publish.
		assert_eq!(track.latest(), None);
	}

	#[test]
	fn a_rejected_edit_leaves_the_window_unchanged() {
		let track = rejecting_track();
		let mut subscriber = track.subscribe(None).ordered();
		let mut producer = Producer::<Value>::new(track, ProducerConfig::default());

		assert!(producer.push(&rec(1)).is_err());
		assert_eq!(producer.range(), 0..0);
		assert!(producer.window().is_empty());

		let waiter = kio::Waiter::noop();
		let Poll::Ready(Ok(Some(mut group))) = subscriber.poll_next_group(&waiter) else {
			panic!("the rejected group's header was published");
		};
		assert!(matches!(group.poll_read_frame(&waiter), Poll::Ready(Ok(None))));
	}

	#[test]
	fn writes_after_another_clone_finishes_are_rejected() {
		let (mut producer, _track) = producer(ProducerConfig::default());
		producer.push(&rec(1)).unwrap();
		producer.clone().finish().unwrap();

		assert!(matches!(
			producer.push(&rec(2)),
			Err(crate::Error::Net(moq_net::Error::Closed))
		));
		assert!(matches!(
			producer.pop(1),
			Err(crate::Error::Net(moq_net::Error::Closed))
		));
		assert_eq!(producer.window(), vec![rec(1)]);
	}

	#[test]
	fn a_pop_is_clamped_to_the_window() {
		let mut live = Live::new(ProducerConfig::default());
		live.push(0);
		live.pop(9);
		live.push(1);

		assert_eq!(
			live.finish(),
			vec![
				Event::Push {
					index: 0,
					value: rec(0)
				},
				Event::Pop(0..1),
				Event::Push {
					index: 1,
					value: rec(1)
				},
			]
		);
	}

	#[test]
	fn a_large_gap_is_one_skip_event() {
		let mut decoder = Decoder::<Value>::new(ConsumerConfig::default());
		let mut group = decoder.group();
		group.decode(br#"{"offset":0,"records":[]}"#).unwrap();
		let mut group = decoder.group();
		group.decode(br#"{"offset":9007199254740991,"records":[]}"#).unwrap();

		assert_eq!(decoder.next_event(), Some(Event::Skip(0..super::encoder::MAX_INDEX)));
		assert_eq!(decoder.next_event(), None);
	}

	#[test]
	fn indices_must_fit_the_shared_safe_integer_range() {
		let mut decoder = Decoder::<Value>::new(ConsumerConfig::default());
		let mut group = decoder.group();
		assert!(group.decode(br#"{"offset":9007199254740992,"records":[]}"#).is_err());

		let mut decoder = Decoder::<Value>::new(ConsumerConfig::default());
		let mut group = decoder.group();
		group.decode(br#"{"offset":9007199254740991,"records":[]}"#).unwrap();
		assert!(group.decode(br#"{"push":null}"#).is_err());
	}

	#[test]
	fn a_checkpoint_cannot_start_before_the_window() {
		let mut decoder = Decoder::<Value>::new(ConsumerConfig::default());
		let mut group = decoder.group();
		assert!(group.decode(br#"{"offset":2,"start":1,"records":[]}"#).is_err());
	}

	#[test]
	fn every_group_requires_a_header() {
		let mut decoder = Decoder::<Value>::new(ConsumerConfig::default());
		let mut group = decoder.group();
		group.decode(br#"{"offset":0,"records":[]}"#).unwrap();
		let mut group = decoder.group();

		assert!(group.decode(br#"{"push":null}"#).is_err());
	}

	#[test]
	fn a_header_is_only_valid_as_frame_zero() {
		let mut decoder = Decoder::<Value>::new(ConsumerConfig::default());
		let mut group = decoder.group();
		group.decode(br#"{"offset":0,"records":[]}"#).unwrap();

		assert!(group.decode(br#"{"offset":0,"records":[]}"#).is_err());
	}

	#[test]
	fn rolling_is_invisible_to_the_consumer() {
		// The same edits, framed two ways: one group for everything, versus a roll per edit.
		let edits = |ratio: u32| {
			let mut live = Live::new(ProducerConfig::default().with_op_ratio(ratio));
			for n in 0..6 {
				live.push(n);
				if n >= 3 {
					live.pop(1);
				}
			}
			live.finish()
		};

		assert_eq!(edits(1_000), edits(0));
	}
}
