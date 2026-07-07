//! Append-log JSON publishing over [`moq-net`](moq_net) tracks.
//!
//! The counterpart to the crate root's snapshot/delta ("object") mode: instead of one JSON
//! value updated over time, a stream is an ordered log of self-contained records. Every
//! [`Producer::append`] writes one JSON object as one frame, and a [`Consumer`] yields every
//! record in order.
//!
//! The whole log rides a **single group** that is never rolled: with
//! [`ProducerConfig::compression`] on, that one group is one DEFLATE window, so every record
//! compresses against all the earlier ones. There is deliberately no group rolling (and so no
//! catch-up machinery): the only reason to roll would be moq-net's per-group frame cap, which
//! isn't worth working around here. A caller that wants to bound the record rate throttles at
//! the source (e.g. the timeline's granularity); a consumer that finds a gap can fetch or
//! extrapolate. A late joiner reads whatever frames the relay still retains for the group;
//! deep history is served from a recording, not this live stream.

use std::marker::PhantomData;
use std::sync::{Arc, Mutex};
use std::task::Poll;

use bytes::Bytes;
use moq_flate::{Decoder, Encoder};
use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::Result;

/// Configuration for a stream [`Producer`].
///
/// Build from [`Default`] and override fields (the struct is `#[non_exhaustive]`, so new
/// options stay additive).
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct ProducerConfig {
	/// Compress the group as one sync-flushed DEFLATE stream, so each record reuses the earlier
	/// ones as context and shrinks sharply.
	///
	/// `false` (the default) writes plaintext JSON frames. A [`Consumer`] reading the track must
	/// set [`ConsumerConfig::compression`] to match.
	pub compression: bool,
}

impl ProducerConfig {
	/// Set [`compression`](Self::compression) (a builder, since the struct is `#[non_exhaustive]`).
	pub fn with_compression(mut self, compression: bool) -> Self {
		self.compression = compression;
		self
	}
}

/// Configuration for a stream [`Consumer`].
///
/// Build from [`Default`] and override fields (the struct is `#[non_exhaustive]`, so new options
/// stay additive).
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct ConsumerConfig {
	/// Whether the track's frames are DEFLATE-compressed. Must match the producer's
	/// [`ProducerConfig::compression`]. Defaults to `false`.
	pub compression: bool,
}

impl ConsumerConfig {
	/// Set [`compression`](Self::compression) (a builder, since the struct is `#[non_exhaustive]`).
	pub fn with_compression(mut self, compression: bool) -> Self {
		self.compression = compression;
		self
	}
}

/// Publishes an ordered log of JSON records over a track, one record per frame in a single group.
///
/// Cheaply clonable: clones share one underlying track and publishing state, so multiple owners
/// (e.g. several producers feeding one log) append into a single ordered stream.
pub struct Producer<T> {
	inner: Arc<Mutex<Inner>>,
	_marker: PhantomData<fn(T)>,
}

impl<T> Clone for Producer<T> {
	fn clone(&self) -> Self {
		Self {
			inner: self.inner.clone(),
			_marker: PhantomData,
		}
	}
}

impl<T> Producer<T> {
	/// Create a subscriber for the underlying track.
	pub fn consume(&self) -> moq_net::TrackConsumer {
		self.inner.lock().unwrap().track.consume()
	}
}

impl<T: Serialize> Producer<T> {
	/// Create a producer that publishes to the given track.
	pub fn new(track: moq_net::TrackProducer, config: ProducerConfig) -> Self {
		Self {
			inner: Arc::new(Mutex::new(Inner {
				track,
				group: None,
				encoder: None,
				config,
			})),
			_marker: PhantomData,
		}
	}

	/// Append one record to the log.
	pub fn append(&mut self, value: &T) -> Result<()> {
		self.inner.lock().unwrap().append(value)
	}

	/// Finish the track, closing the group.
	pub fn finish(&mut self) -> Result<()> {
		self.inner.lock().unwrap().finish()
	}
}

/// Shared publishing state behind [`Producer`]'s `Arc<Mutex>`.
struct Inner {
	track: moq_net::TrackProducer,
	// The single group carrying the whole log, opened on the first append.
	group: Option<moq_net::GroupProducer>,
	// The group's DEFLATE encoder (one window for the whole log), `Some` while compressing.
	encoder: Option<Encoder>,
	config: ProducerConfig,
}

impl Inner {
	fn append<T: Serialize>(&mut self, value: &T) -> Result<()> {
		let payload = Bytes::from(serde_json::to_vec(value)?);

		if self.group.is_none() {
			self.group = Some(self.track.append_group()?);
			self.encoder = self.config.compression.then(Encoder::new);
		}

		let slice = match self.encoder.as_mut() {
			Some(encoder) => encoder.frame(&payload),
			None => payload,
		};
		self.group.as_mut().expect("a group is open").write_frame(slice)?;
		Ok(())
	}

	fn finish(&mut self) -> Result<()> {
		if let Some(mut group) = self.group.take() {
			group.finish()?;
		}
		self.track.finish()?;
		Ok(())
	}
}

/// Consumes an ordered log of JSON records from a track, yielding every record in order.
///
/// The log rides a single group, so this reads that group's frames in order; one record per frame.
pub struct Consumer<T> {
	track: moq_net::TrackConsumer,
	group: Option<moq_net::GroupConsumer>,
	compressed: bool,
	// The group's DEFLATE decoder (one window for the whole log), built on the first frame.
	decoder: Option<Decoder>,
	_marker: PhantomData<fn() -> T>,
}

impl<T: DeserializeOwned> Consumer<T> {
	/// Create a consumer reading from the given track subscriber.
	///
	/// Set [`ConsumerConfig::compression`] to read a track written by a producer with
	/// [`ProducerConfig::compression`] on.
	pub fn new(track: moq_net::TrackConsumer, config: ConsumerConfig) -> Self {
		Self {
			track,
			group: None,
			compressed: config.compression,
			decoder: None,
			_marker: PhantomData,
		}
	}

	/// Get the next record, or `None` once the track ends.
	pub async fn next(&mut self) -> Result<Option<T>>
	where
		T: Unpin,
	{
		kio::wait(|waiter| self.poll_next(waiter)).await
	}

	/// Poll for the next record, without blocking.
	pub fn poll_next(&mut self, waiter: &kio::Waiter) -> Poll<Result<Option<T>>> {
		loop {
			let Some(group) = &mut self.group else {
				match self.track.poll_next_group(waiter)? {
					Poll::Ready(Some(group)) => {
						self.decoder = self.compressed.then(Decoder::new);
						self.group = Some(group);
						continue;
					}
					Poll::Ready(None) => return Poll::Ready(Ok(None)),
					Poll::Pending => return Poll::Pending,
				}
			};

			match group.poll_read_frame(waiter)? {
				Poll::Ready(Some(frame)) => {
					let plain = match self.decoder.as_mut() {
						Some(decoder) => decoder.frame(&frame)?,
						None => frame,
					};
					return Poll::Ready(Ok(Some(serde_json::from_slice(&plain)?)));
				}
				Poll::Ready(None) => {
					// The group is finished; the log rides just this one, so the next poll for a
					// group ends the stream.
					self.group = None;
					self.decoder = None;
				}
				Poll::Pending => return Poll::Pending,
			}
		}
	}
}

#[cfg(test)]
mod test {
	use super::*;
	use serde_json::{Value, json};

	fn producer(config: ProducerConfig) -> (Producer<Value>, moq_net::TrackConsumer) {
		let track = moq_net::Track::new("test").produce();
		let consumer = track.consume();
		(Producer::new(track, config), consumer)
	}

	fn compressed() -> ProducerConfig {
		ProducerConfig { compression: true }
	}

	fn consumer(track: moq_net::TrackConsumer, compression: bool) -> Consumer<Value> {
		Consumer::new(track, ConsumerConfig { compression })
	}

	/// Drain every record currently available without blocking.
	fn drain(mut consumer: Consumer<Value>) -> Vec<Value> {
		let waiter = kio::Waiter::noop();
		let mut out = Vec::new();
		while let Poll::Ready(Ok(Some(value))) = consumer.poll_next(&waiter) {
			out.push(value);
		}
		out
	}

	#[test]
	fn plaintext_roundtrip_in_order() {
		let (mut producer, track) = producer(ProducerConfig::default());
		for n in 0..5 {
			producer.append(&json!({ "n": n })).unwrap();
		}
		producer.finish().unwrap();

		let records = drain(consumer(track, false));
		assert_eq!(records, (0..5).map(|n| json!({ "n": n })).collect::<Vec<_>>());
	}

	#[test]
	fn compressed_roundtrip_in_order() {
		let (mut producer, track) = producer(compressed());
		for n in 0..20 {
			producer.append(&json!({ "group": n, "pts": n * 2_000 })).unwrap();
		}
		producer.finish().unwrap();

		let records = drain(consumer(track, true));
		assert_eq!(records.len(), 20);
		assert_eq!(records[7], json!({ "group": 7, "pts": 14_000 }));
	}

	#[test]
	fn all_records_ride_one_group() {
		let (mut producer, track) = producer(compressed());
		for n in 0..50 {
			producer.append(&json!({ "n": n })).unwrap();
		}
		producer.finish().unwrap();

		// Never rolled: a single group holds the whole log.
		assert_eq!(track.latest(), Some(0));
		assert_eq!(drain(consumer(track, true)).len(), 50);
	}

	#[test]
	fn live_consumer_sees_each_record() {
		let (mut producer, track) = producer(compressed());
		let mut consumer = consumer(track, true);
		let waiter = kio::Waiter::noop();

		for n in 0..3 {
			producer.append(&json!({ "n": n })).unwrap();
			match consumer.poll_next(&waiter) {
				Poll::Ready(Ok(Some(value))) => assert_eq!(value, json!({ "n": n })),
				other => panic!("expected record, got {other:?}"),
			}
		}
		assert!(matches!(consumer.poll_next(&waiter), Poll::Pending));
		producer.finish().unwrap();
	}

	#[test]
	fn shared_window_shrinks_repetitive_records() {
		let (mut producer, mut track) = producer(compressed());
		for n in 0..8 {
			producer.append(&json!({ "group": n, "pts": n * 2_000 })).unwrap();
		}
		producer.finish().unwrap();

		let waiter = kio::Waiter::noop();
		let Poll::Ready(Ok(Some(mut group))) = track.poll_next_group(&waiter) else {
			panic!("expected a group");
		};
		let mut sizes = Vec::new();
		while let Poll::Ready(Ok(Some(frame))) = group.poll_read_frame(&waiter) {
			sizes.push(frame.len());
		}
		assert_eq!(sizes.len(), 8);
		let raw = serde_json::to_vec(&json!({ "group": 7, "pts": 14_000 })).unwrap().len();
		assert!(
			*sizes.last().unwrap() < raw / 2,
			"windowed record {} should be far below its raw size {raw}",
			sizes.last().unwrap()
		);
	}

	#[test]
	fn embedded_newlines_survive() {
		// Each record is its own frame (one JSON object), and JSON escapes control characters, so a
		// string value containing a newline round-trips cleanly.
		let (mut producer, track) = producer(compressed());
		let value = json!({ "s": "line1\nline2\ttab", "u": "a\u{000a}b" });
		for _ in 0..4 {
			producer.append(&value).unwrap();
		}
		producer.finish().unwrap();

		let records = drain(consumer(track, true));
		assert_eq!(records, vec![value.clone(), value.clone(), value.clone(), value]);
	}
}
