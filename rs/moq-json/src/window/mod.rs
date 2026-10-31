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
//! instead of a roll that restates the whole window (and, on a
//! [`compress`](moq_net::track::Info::compress)ed track, throws away the group's DEFLATE window).
//!
//! # Group boundaries are invisible
//!
//! The publisher rolls a group when a joiner wants a fresh one, exactly as
//! [`snapshot`](crate::snapshot) does: on a track created with a
//! [`refresh`](moq_net::track::Info::refresh) ratio, a subscriber joining once the open group's ops
//! outweigh that ratio times its header asks for one, and [`Producer::refresh`] restates the window
//! in it. Otherwise a group runs to the frame and byte caps. A [`Consumer`] never surfaces a roll:
//! a header restating records a reader already has yields nothing, so however often the publisher
//! rolls, the reader sees one continuous stream of [`Event`]s.
//! [`ProducerConfig::checkpoint_records`] bounds the suffix repeated on each roll for a long-lived
//! window.
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

#[cfg(test)]
mod properties;

pub use consumer::Consumer;
pub use decoder::{ConsumerConfig, Decoder, Event, Group};
pub use encoder::{Encoded, Encoder, Pending, ProducerConfig};
pub use producer::Producer;

#[cfg(test)]
use futures::FutureExt;

#[cfg(test)]
mod test {
	use std::task::Poll;

	use serde_json::{Value, json};

	use super::*;

	#[test]
	fn pushes_and_pops_keep_the_callers_timestamps() {
		let (mut producer, mut groups) = producer_on(ProducerConfig::default(), timed());
		let first = moq_net::Timestamp::from_millis(100).unwrap();
		let second = moq_net::Timestamp::from_millis(200).unwrap();
		producer
			.push(moq_net::Timed::from(&json!({ "n": 1 })).at(first))
			.unwrap();
		producer.pop(moq_net::Timed::from(1u64).at(second)).unwrap();
		let waiter = kio::Waiter::noop();
		let Poll::Ready(Ok(Some(mut group))) = groups.poll_recv_group(&waiter) else {
			panic!("expected the window group");
		};
		for at in [first, second] {
			let Poll::Ready(Ok(Some(frame))) = group.poll_read_frame(&waiter) else {
				panic!("expected the timestamped window edit");
			};
			assert_eq!(frame.timestamp, Some(at));
		}
	}

	/// Refused before the header's group is published, so the window carries on cleanly.
	#[test]
	fn a_timed_track_refuses_a_bare_publish() {
		let (mut producer, mut track) = producer_on(ProducerConfig::default(), timed());
		assert!(matches!(
			producer.push(&json!({ "n": 0 })),
			Err(crate::Error::Net(moq_net::Error::TimestampMismatch))
		));
		assert_eq!(
			track.largest().now_or_never().flatten().map(|position| position.group),
			None,
			"a refused header published a group"
		);
		let at = moq_net::Timestamp::from_millis(100).unwrap();
		producer.push(moq_net::Timed::from(&rec(1)).at(at)).unwrap();
		producer.push(moq_net::Timed::from(&rec(2)).at(at)).unwrap();
		producer.finish().unwrap();
		let pushed: Vec<_> = drain(&mut consumer(track))
			.into_iter()
			.filter_map(|event| match event {
				Event::Push { value, .. } => Some(value),
				_ => None,
			})
			.collect();
		assert_eq!(pushed, vec![rec(1), rec(2)]);
	}

	#[test]
	fn events_carry_their_frames_timestamp() {
		let track = moq_net::broadcast::Info::new()
			.produce()
			.create_track("test", timed())
			.unwrap();
		let mut live = consumer(track.subscribe(None));
		let late = track.subscribe(None);
		let mut producer = Producer::new(track, ProducerConfig::default());
		let at = |ms| moq_net::Timestamp::from_millis(ms).unwrap();

		producer.push(moq_net::Timed::from(&rec(1)).at(at(100))).unwrap();
		producer.push(moq_net::Timed::from(&rec(2)).at(at(200))).unwrap();
		producer.pop(moq_net::Timed::from(1u64).at(at(300))).unwrap();
		let events: Vec<_> = drain_timed(&mut live).into_iter().map(|e| (e.value, e.at)).collect();
		assert_eq!(
			events,
			vec![
				(
					Event::Push {
						index: 0,
						value: rec(1)
					},
					Some(at(100))
				),
				(
					Event::Push {
						index: 1,
						value: rec(2)
					},
					Some(at(200))
				),
				(Event::Pop(0..1), Some(at(300))),
			]
		);

		// A reader joining at a later group learns of the retained record from its header, so it
		// gets the header frame's time rather than the record's original push time.
		producer.cut().unwrap();
		producer.push(moq_net::Timed::from(&rec(3)).at(at(400))).unwrap();
		let late = late.with_range(1..);
		let mut late = consumer(late);
		let events: Vec<_> = drain_timed(&mut late).into_iter().map(|e| (e.value, e.at)).collect();
		assert_eq!(
			events,
			vec![
				(
					Event::Push {
						index: 1,
						value: rec(2)
					},
					Some(at(400))
				),
				(
					Event::Push {
						index: 2,
						value: rec(3)
					},
					Some(at(400))
				),
			]
		);
	}

	#[test]
	fn untimed_events_have_no_timestamp() {
		let (mut producer, track) = producer(ProducerConfig::default());
		producer.push(&rec(1)).unwrap();
		producer.pop(1).unwrap();
		let events = drain_timed(&mut consumer(track));
		assert_eq!(events.len(), 2);
		assert!(events.iter().all(|event| event.at.is_none()));
	}

	#[test]
	fn a_bare_publish_stays_untimed() {
		let track = moq_net::broadcast::Info::new()
			.produce()
			.create_track("untimed", moq_net::track::Info::default())
			.unwrap();
		let mut groups = track.subscribe(None);
		let mut producer = Producer::new(track, ProducerConfig::default());
		producer.push(&json!({ "n": 1 })).unwrap();
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

	fn producer(config: ProducerConfig) -> (Producer<Value>, moq_net::track::Subscriber) {
		producer_on(config, moq_net::track::Info::default())
	}

	fn producer_on(
		config: ProducerConfig,
		info: moq_net::track::Info,
	) -> (Producer<Value>, moq_net::track::Subscriber) {
		let track = moq_net::broadcast::Info::new()
			.produce()
			.create_track("test", info)
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

	fn consumer(track: moq_net::track::Subscriber) -> Consumer<Value> {
		Consumer::new(track, ConsumerConfig::default())
	}

	/// A track whose timestamp conversion rejects every frame after its group is published.
	fn rejecting_track() -> moq_net::track::Producer {
		let mut info = timed();
		info.timescale = Some(moq_net::Timescale::new((1u64 << 62) - 1).unwrap());

		moq_net::broadcast::Info::new()
			.produce()
			.create_track("test", Some(info))
			.unwrap()
	}

	/// Drain every event currently available without blocking.
	fn drain(consumer: &mut Consumer<Value>) -> Vec<Event<Value>> {
		drain_timed(consumer).into_iter().map(|event| event.value).collect()
	}

	/// Drain every event currently available, with its frame timestamp.
	fn drain_timed(consumer: &mut Consumer<Value>) -> Vec<moq_net::Timed<Event<Value>>> {
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
			Self::on(config, moq_net::track::Info::default())
		}

		fn on(config: ProducerConfig, info: moq_net::track::Info) -> Self {
			let (producer, track) = producer_on(config, info);
			Self {
				producer,
				consumer: consumer(track),
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
				mut producer,
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
		let mut live = Live::new(ProducerConfig::default().with_ops(false));
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
		let config = ProducerConfig::default().with_ops(false).with_checkpoint_records(2);
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
		let config = ProducerConfig::default().with_ops(false).with_checkpoint_records(2);
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
			.create_track("test", moq_net::track::Info::default())
			.unwrap();
		let mut producer = Producer::<Value>::new(track, ProducerConfig::default().with_ops(false));

		for n in 0..5 {
			producer.push(&rec(n)).unwrap();
		}
		producer.pop(3).unwrap();
		let mut subscriber = producer.consume();
		let largest = subscriber
			.largest()
			.now_or_never()
			.flatten()
			.map(|position| position.group)
			.unwrap();
		subscriber = subscriber.with_range(largest..);
		let mut fresh = consumer(subscriber);
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
		let mut encoder = Encoder::<Value>::new(ProducerConfig::default().with_ops(false));
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
		// Each loss as this side raises it, then as a publisher's reset carries it.
		for err in [
			moq_net::Error::Old,
			moq_net::Error::Lagged,
			moq_net::Error::Evicted,
			moq_net::Error::GroupTooLarge,
			moq_net::Error::Stream(moq_net::StreamError::Old),
			moq_net::Error::Stream(moq_net::StreamError::TooFarBehind),
			moq_net::Error::Stream(moq_net::StreamError::Evicted),
			moq_net::Error::Stream(moq_net::StreamError::GroupTooLarge),
		] {
			let track = moq_net::broadcast::Info::new()
				.produce()
				.create_track("test", timed())
				.unwrap();
			let mut consumer = consumer(track.subscribe(None));
			let mut encoder = Encoder::<Value>::new(ProducerConfig::default().with_ops(false));

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

	/// A joiner under the ratio reads the open group, and one over it gets the window restated in a
	/// fresh group, where it reads the same records with none of the ops before it.
	#[test]
	fn a_joiner_over_the_ratio_gets_a_fresh_header() {
		let waiter = kio::Waiter::noop();
		let (mut producer, _track) = producer_on(
			ProducerConfig::default(),
			moq_net::track::Info::default().with_refresh(1),
		);

		// A 32 byte header and a 16 byte push: under the ratio.
		producer.push(&rec(0)).unwrap();
		producer.push(&rec(1)).unwrap();
		let _early = producer.consume();
		assert!(producer.poll_refresh(&waiter).is_pending(), "a join under the ratio");

		producer.push(&rec(2)).unwrap();
		producer.pop(1).unwrap();
		assert!(producer.poll_refresh(&waiter).is_pending(), "no join, no refresh");
		let mut late = producer.consume();
		assert!(matches!(producer.poll_refresh(&waiter), Poll::Ready(Ok(()))));
		assert!(producer.poll_refresh(&waiter).is_pending(), "answered once");

		let mut fresh = std::iter::from_fn(|| match late.poll_recv_group(&waiter) {
			Poll::Ready(Ok(Some(group))) => Some(group),
			_ => None,
		})
		.last()
		.expect("the fresh group");
		assert_eq!(fresh.sequence, 1);
		let Poll::Ready(Ok(Some(header))) = fresh.poll_read_frame(&waiter) else {
			panic!("expected the restating header");
		};
		assert_eq!(
			serde_json::from_slice::<Value>(&header.payload).unwrap(),
			json!({ "offset": 1, "records": [rec(1), rec(2)] })
		);

		// The next edit rides as an op in the fresh group.
		producer.push(&rec(3)).unwrap();
		let Poll::Ready(Ok(Some(op))) = fresh.poll_read_frame(&waiter) else {
			panic!("expected an op in the fresh group");
		};
		assert_eq!(
			serde_json::from_slice::<Value>(&op.payload).unwrap(),
			json!({ "push": rec(3) })
		);
	}

	/// A refresh restates the window at the newest timestamp written.
	#[test]
	fn a_refresh_reuses_the_newest_timestamp() {
		let waiter = kio::Waiter::noop();
		let ms = |n| moq_net::Timestamp::from_millis(n).unwrap();
		let (mut producer, _track) = producer_on(ProducerConfig::default(), timed().with_refresh(1));
		for n in 0..4 {
			producer.push(moq_net::Timed::from(&rec(n)).at(ms(100 + n))).unwrap();
		}
		producer.pop(moq_net::Timed::from(1).at(ms(200))).unwrap();

		let mut late = producer.consume();
		assert!(matches!(producer.poll_refresh(&waiter), Poll::Ready(Ok(()))));
		let mut fresh = std::iter::from_fn(|| match late.poll_recv_group(&waiter) {
			Poll::Ready(Ok(Some(group))) => Some(group),
			_ => None,
		})
		.last()
		.expect("the fresh group");
		let Poll::Ready(Ok(Some(header))) = fresh.poll_read_frame(&waiter) else {
			panic!("expected the restating header");
		};
		assert_eq!(header.timestamp, Some(ms(200)));
	}

	/// An op may carry a timestamp older than its group's first frame, but a new group may not start
	/// before that frame, so the restating header takes the newest timestamp written.
	#[test]
	fn a_refresh_after_an_older_edit_starts_at_the_newest_timestamp() {
		let waiter = kio::Waiter::noop();
		let ms = |n| moq_net::Timestamp::from_millis(n).unwrap();
		let (mut producer, _track) = producer_on(ProducerConfig::default(), timed().with_refresh(1));
		for (n, at) in [130, 110, 120, 115].into_iter().enumerate() {
			producer.push(moq_net::Timed::from(&rec(n as u64)).at(ms(at))).unwrap();
		}

		let mut late = producer.consume();
		assert!(matches!(producer.poll_refresh(&waiter), Poll::Ready(Ok(()))));
		let mut fresh = std::iter::from_fn(|| match late.poll_recv_group(&waiter) {
			Poll::Ready(Ok(Some(group))) => Some(group),
			_ => None,
		})
		.last()
		.expect("the fresh group");
		let Poll::Ready(Ok(Some(header))) = fresh.poll_read_frame(&waiter) else {
			panic!("expected the restating header");
		};
		assert_eq!(header.timestamp, Some(ms(130)));
	}

	/// Without a ratio no join asks for a group, and a finished producer stops refreshing.
	#[test]
	fn refresh_needs_a_ratio_and_an_open_track() {
		let (mut producer, _track) = producer(ProducerConfig::default());
		for n in 0..8 {
			producer.push(&rec(n)).unwrap();
		}
		let _late = producer.consume();
		assert!(producer.poll_refresh(&kio::Waiter::noop()).is_pending());

		producer.finish().unwrap();
		assert!(matches!(producer.refresh().now_or_never(), Some(Err(_))));
	}

	#[test]
	fn compressed_round_trip_across_rolls() {
		let mut live = Live::on(
			ProducerConfig::default(),
			moq_net::track::Info::default().with_compress(true),
		);
		for n in 0..40 {
			live.push(n);
			if n >= 10 {
				live.pop(1);
			}
			if n % 7 == 6 {
				live.producer.cut().unwrap();
			}
		}

		// Cuts roll many compressed groups; every record still arrives exactly once, in order.
		assert_eq!(Live::pushed(&live.finish()), (0..40).collect::<Vec<_>>());
	}

	#[test]
	fn an_empty_pop_writes_nothing() {
		let (mut producer, mut track) = producer(ProducerConfig::default());
		producer.pop(5).unwrap();
		producer.finish().unwrap();

		// Nothing was ever pushed, so there is nothing to drop and no group to publish.
		assert_eq!(
			track.largest().now_or_never().flatten().map(|position| position.group),
			None
		);
	}

	#[test]
	fn a_rejected_edit_leaves_the_window_unchanged() {
		let track = rejecting_track();
		let mut subscriber = track.subscribe(None).ordered();
		let mut producer = Producer::<Value>::new(track, ProducerConfig::default());

		// Too late for the timescale, so the header group is published before the write fails.
		let late = moq_net::Timestamp::from_millis(10_000).unwrap();
		assert!(producer.push(moq_net::Timed::from(&rec(1)).at(late)).is_err());
		assert_eq!(producer.range(), 0..0);
		assert!(producer.window().is_empty());

		let waiter = kio::Waiter::noop();
		let Poll::Ready(Ok(Some(mut group))) = subscriber.poll_next_group(&waiter) else {
			panic!("the rejected group's header was published");
		};
		assert!(matches!(group.poll_read_frame(&waiter), Poll::Ready(Ok(None))));
	}

	#[test]
	fn the_handle_remains_usable_after_finish() {
		let (mut producer, mut track) = producer(ProducerConfig::default());
		producer.push(&rec(1)).unwrap();
		producer.finish().unwrap();

		assert_eq!(producer.window(), vec![rec(1)]);
		assert_eq!(producer.range(), 0..1);
		let mut mirror = producer.consume();
		assert_eq!(
			mirror.largest().now_or_never().flatten().map(|position| position.group),
			track.largest().now_or_never().flatten().map(|position| position.group)
		);
		producer.finish().unwrap();
		assert!(matches!(
			producer.push(&rec(2)),
			Err(crate::Error::Net(moq_net::Error::Closed))
		));
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
	fn cut_finishes_the_group_and_the_next_edit_restates() {
		let track = moq_net::broadcast::Info::new()
			.produce()
			.create_track("test", moq_net::track::Info::default())
			.unwrap();
		let replay = moq_net::track::Subscription::default().with_max_delay(std::time::Duration::from_secs(30));
		let mut groups = track.subscribe(replay);
		let mut producer = Producer::<Value>::new(track, ProducerConfig::default());

		producer.cut().unwrap();
		producer.push(&rec(0)).unwrap();
		producer.push(&rec(1)).unwrap();
		producer.cut().unwrap();
		producer.cut().unwrap();
		producer.push(&rec(2)).unwrap();

		let waiter = kio::Waiter::noop();
		let Poll::Ready(Ok(Some(mut first))) = groups.poll_recv_group(&waiter) else {
			panic!("the first group is published");
		};
		assert_eq!(first.frame_count(), 2);
		assert!(first.poll_finished(&waiter).is_ready(), "cut finishes the open group");

		let Poll::Ready(Ok(Some(mut second))) = groups.poll_recv_group(&waiter) else {
			panic!("the next edit opens a group");
		};
		let Poll::Ready(Ok(Some(header))) = second.poll_read_frame(&waiter) else {
			panic!("the group starts with a header");
		};
		let header: Value = serde_json::from_slice(&header.payload).unwrap();
		assert_eq!(header, json!({ "offset": 0, "records": [rec(0), rec(1), rec(2)] }));
		assert!(groups.poll_recv_group(&waiter).is_pending());
	}

	#[test]
	fn trailing_data_after_a_frame_is_rejected() {
		let mut decoder = Decoder::<Value>::new(ConsumerConfig::default());
		let mut group = decoder.group();
		assert!(group.decode(br#"{"offset":0,"records":[]} {}"#).is_err());

		group.decode(br#"{"offset":0,"records":[]}"#).unwrap();
		assert!(group.decode(br#"{"push":1}junk"#).is_err());
		assert!(group.decode(br#"{"pop":0}{"pop":0}"#).is_err());
		assert_eq!(group.range(), 0..0);
	}

	/// A repeated key is refused in a header, in a record, and in the op object itself.
	#[test]
	fn duplicate_keys_are_refused() {
		let vectors = crate::strict::test::vectors();
		for record in &vectors.refused {
			let mut decoder = Decoder::<Value>::new(ConsumerConfig::default());
			let mut group = decoder.group();
			let header = format!(r#"{{"offset":0,"records":[{record}]}}"#);
			let err = group.decode(header.as_bytes()).unwrap_err();
			assert!(matches!(err, crate::Error::DuplicateKey), "{header}: {err}");

			group.decode(br#"{"offset":0,"records":[]}"#).unwrap();
			let push = format!(r#"{{"push":{record}}}"#);
			let err = group.decode(push.as_bytes()).unwrap_err();
			assert!(matches!(err, crate::Error::DuplicateKey), "{push}: {err}");
		}
		for record in &vectors.accepted {
			let mut decoder = Decoder::<Value>::new(ConsumerConfig::default());
			let mut group = decoder.group();
			group
				.decode(format!(r#"{{"offset":0,"records":[{record}]}}"#).as_bytes())
				.unwrap();
			group.decode(format!(r#"{{"push":{record}}}"#).as_bytes()).unwrap();
		}

		let mut decoder = Decoder::<Value>::new(ConsumerConfig::default());
		let mut group = decoder.group();
		let err = group.decode(br#"{"offset":0,"offset":0,"records":[]}"#).unwrap_err();
		assert!(matches!(err, crate::Error::DuplicateKey), "{err}");
		group.decode(br#"{"offset":0,"records":[]}"#).unwrap();
		let err = group.decode(br#"{"pop":0,"pop":0}"#).unwrap_err();
		assert!(matches!(err, crate::Error::DuplicateKey), "{err}");
	}

	#[test]
	fn rolling_is_invisible_to_the_consumer() {
		// The same edits, framed two ways: one group for everything, versus a roll per edit.
		let edits = |ops: bool| {
			let mut live = Live::new(ProducerConfig::default().with_ops(ops));
			for n in 0..6 {
				live.push(n);
				if n >= 3 {
					live.pop(1);
				}
			}
			live.finish()
		};

		assert_eq!(edits(true), edits(false));
	}
}
