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
//! Every frame is a tagged op. Each group opens with a `reset` naming the retained records and the
//! absolute `offset` of the first, followed by `push` and `pop` ops. Only the reset carries an
//! index: a push takes the next one and a pop drops from the front, both positional against the
//! reset, which is sound because the group-scoped DEFLATE window already makes a mid-group join
//! undecodable.
//!
//! Trimming is therefore an op, not a group boundary. Dropping a record costs one small frame
//! inside the shared compression window instead of a roll that would throw that window away.
//!
//! # Group boundaries are invisible
//!
//! The publisher rolls a group when the ops in it outgrow
//! [`ProducerConfig::op_ratio`](ProducerConfig::op_ratio) times the reset that opened it, exactly as
//! [`snapshot`](crate::snapshot) rolls on its delta budget. That is purely a compression decision:
//! there is no caller-driven cut and no age bound, and a [`Consumer`] never surfaces it. A reset
//! restating records a reader already has yields nothing, so however often the publisher rolls, the
//! reader sees one continuous stream of [`Event`]s.
//!
//! # What a reader is told
//!
//! Every index is reported exactly once: [`Event::Push`] when a record arrives, [`Event::Pop`] when
//! it leaves, and [`Event::Skip`] when it existed but was dropped before this reader saw it. A
//! reader that keeps up sees pushes and pops; one that joins late, or falls a group behind, learns
//! from the reset's offset which records it will never get, rather than silently missing them.
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
pub use decoder::{ConsumerConfig, Decoder, Event};
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

	/// A subscription patient enough to read every group of a finished track.
	fn replay(track: &moq_net::track::Producer) -> moq_net::track::Subscriber {
		track.subscribe(moq_net::track::Subscription::default().with_max_age(std::time::Duration::from_secs(30)))
	}

	fn consumer(track: moq_net::track::Subscriber, compression: bool) -> Consumer<Value> {
		Consumer::new(track, ConsumerConfig::default().with_compression(compression))
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
	/// exists, so it would resume at the newest reset instead of reading the rolls in between.
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

		fn finish(mut self) -> Vec<Event<Value>> {
			self.producer.finish().unwrap();
			self.read();
			self.events
		}

		/// Just the indices pushed, in order.
		fn pushed(events: &[Event<Value>]) -> Vec<u64> {
			events
				.iter()
				.filter_map(|e| match e {
					Event::Push(i, _) => Some(*i),
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
				Event::Push(0, rec(0)),
				Event::Push(1, rec(1)),
				Event::Pop(0),
				Event::Push(2, rec(2)),
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

		// Three pushes past the first pop, so the window holds the last three and starts at 2.
		assert_eq!(producer.range(), 3..5);
		assert_eq!(producer.window(), vec![rec(3), rec(4)]);
	}

	#[test]
	fn a_popped_record_is_never_restated() {
		// Ops disabled, so every single edit is its own reset restating the whole window.
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
				Event::Push(0, rec(0)),
				Event::Push(1, rec(1)),
				Event::Pop(0),
				Event::Push(2, rec(2)),
			]
		);
	}

	#[test]
	fn a_fresh_consumer_adopts_the_offset_without_skipping_history() {
		let track = moq_net::broadcast::Info::new()
			.produce()
			.create_track("test", None)
			.unwrap();
		let replay = replay(&track);
		let mut producer = Producer::<Value>::new(track, ProducerConfig::default());

		for n in 0..5 {
			producer.push(&rec(n)).unwrap();
		}
		producer.pop(3).unwrap();
		producer.finish().unwrap();

		// Joining at offset 3 must not report 3 skips for records that were never this reader's to
		// miss: it simply starts where the window starts.
		let events = drain(&mut consumer(replay, false));
		assert_eq!(events.first(), Some(&Event::Push(0, rec(0))));
		assert!(!events.iter().any(|e| matches!(e, Event::Skip(_))));
	}

	#[test]
	fn a_lagging_consumer_is_told_what_it_missed() {
		// Ops disabled, so every edit rolls: a reader that stops polling really does lose groups.
		let (mut producer, track) = producer(ProducerConfig::default().with_op_ratio(0));
		let mut consumer = consumer(track, false);

		producer.push(&rec(0)).unwrap();
		producer.push(&rec(1)).unwrap();
		assert_eq!(
			drain(&mut consumer),
			vec![Event::Push(0, rec(0)), Event::Push(1, rec(1))]
		);

		// The consumer stops reading. The window slides past everything it holds, and the publisher
		// rolls, so the groups it would have read are gone.
		for n in 2..8 {
			producer.push(&rec(n)).unwrap();
			producer.pop(1).unwrap();
		}
		producer.finish().unwrap();

		let events = drain(&mut consumer);
		let skipped: Vec<u64> = events
			.iter()
			.filter_map(|e| match e {
				Event::Skip(i) => Some(*i),
				_ => None,
			})
			.collect();

		// Records 2..=5 existed but this reader will never receive them, and it is told so rather than
		// silently jumping from 1 to 6.
		assert!(!skipped.is_empty(), "expected skips, got {events:?}");
		assert_eq!(skipped.first(), Some(&2));

		// Every index is still accounted for exactly once, in order.
		let reported: Vec<u64> = events
			.iter()
			.filter_map(|e| match e {
				Event::Push(i, _) | Event::Skip(i) => Some(*i),
				Event::Pop(_) => None,
			})
			.collect();
		assert!(reported.windows(2).all(|w| w[1] == w[0] + 1), "gaps in {reported:?}");
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
	fn a_pop_is_clamped_to_the_window() {
		let mut live = Live::new(ProducerConfig::default());
		live.push(0);
		live.pop(9);
		live.push(1);

		assert_eq!(
			live.finish(),
			vec![Event::Push(0, rec(0)), Event::Pop(0), Event::Push(1, rec(1))]
		);
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
