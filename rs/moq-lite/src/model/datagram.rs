//! Datagrams are an unreliable, unordered delivery path on a [`super::Track`].
//!
//! A [DatagramsProducer] writes datagrams that are cached briefly (see
//! [`MAX_DATAGRAM_AGE`]) and fanned out to any [DatagramsConsumer]s. Each
//! consumer can read at its own pace, optionally filtering out datagrams older
//! than a configurable `max_latency`.
//!
//! Wire counterpart: [`crate::lite::Datagram`].

use std::collections::VecDeque;
use std::task::{Poll, ready};
use std::time::Duration;

use bytes::Bytes;

use crate::{Error, Result, coding};

/// Datagrams older than this are evicted from the cache.
pub const MAX_DATAGRAM_AGE: Duration = Duration::from_millis(33);

/// Maximum payload size per datagram, in bytes.
pub const MAX_DATAGRAM_PAYLOAD: usize = 1200;

/// A single datagram: opaque payload with a sequence number.
///
/// moq-lite-04-datagrams ignores the sequence number for delivery semantics; the field is
/// preserved so the same model works under a future moq-transport adapter where
/// sequence is meaningful.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Datagram {
	pub sequence: u64,
	pub payload: Bytes,
}

/// Per-subscriber datagrams subscription preferences.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DatagramsSubscription {
	/// Maximum tolerated cache age. `Duration::ZERO` means strict: the
	/// publisher's serve loop should skip past existing cache entries (via
	/// [`DatagramsConsumer::skip_to_latest`]) and only forward fresh arrivals;
	/// dropped sends (e.g. congestion-controller backpressure) are not retried.
	/// Non-zero values let the subscriber receive cached entries up to that age.
	pub max_latency: Duration,
}

#[derive(Default)]
struct State {
	/// Datagrams in arrival order, paired with their write timestamps.
	datagrams: VecDeque<(Datagram, tokio::time::Instant)>,

	/// The number of datagrams evicted from the front of the ring.
	offset: usize,

	/// The highest sequence number written so far (used by `append` for auto-increment).
	max_sequence: Option<u64>,

	/// Set when the producer marks the channel finished.
	closed: bool,

	/// The error that caused the channel to abort, if any.
	abort: Option<Error>,
}

impl State {
	/// Find the next datagram at or after `index` in arrival order whose age
	/// satisfies `max_latency`. Returns the datagram and its absolute index so
	/// the consumer can advance past it.
	///
	/// `max_latency == Duration::ZERO` is strict: caller-controlled (the
	/// consumer should typically advance past stale entries before calling).
	/// For non-zero `max_latency`, datagrams older than that are skipped.
	fn poll_recv(&self, index: usize, max_latency: Duration) -> Poll<Result<Option<(Datagram, usize)>>> {
		let now = tokio::time::Instant::now();
		let start = index.saturating_sub(self.offset);

		for (i, (datagram, written_at)) in self.datagrams.iter().enumerate().skip(start) {
			let age = now.saturating_duration_since(*written_at);
			if !max_latency.is_zero() && age > max_latency {
				continue;
			}
			return Poll::Ready(Ok(Some((datagram.clone(), self.offset + i))));
		}

		if self.closed {
			Poll::Ready(Ok(None))
		} else if let Some(err) = &self.abort {
			Poll::Ready(Err(err.clone()))
		} else {
			Poll::Pending
		}
	}

	fn poll_closed(&self) -> Poll<Result<()>> {
		if self.closed {
			Poll::Ready(Ok(()))
		} else if let Some(err) = &self.abort {
			Poll::Ready(Err(err.clone()))
		} else {
			Poll::Pending
		}
	}

	fn evict_expired(&mut self, now: tokio::time::Instant) {
		while let Some((_, written_at)) = self.datagrams.front() {
			if now.saturating_duration_since(*written_at) <= MAX_DATAGRAM_AGE {
				break;
			}
			self.datagrams.pop_front();
			self.offset += 1;
		}
	}
}

/// Writes datagrams to the shared ring.
pub struct DatagramsProducer {
	state: conducer::Producer<State>,
}

impl DatagramsProducer {
	pub fn new() -> Self {
		Self {
			state: conducer::Producer::default(),
		}
	}

	/// Append a datagram with an explicit sequence number.
	pub fn write(&mut self, datagram: Datagram) -> Result<()> {
		if datagram.payload.len() > MAX_DATAGRAM_PAYLOAD {
			return Err(Error::WrongSize);
		}
		let mut state = self.modify()?;
		if state.closed {
			return Err(Error::Closed);
		}
		let now = tokio::time::Instant::now();
		state.max_sequence = Some(match state.max_sequence {
			Some(prev) => prev.max(datagram.sequence),
			None => datagram.sequence,
		});
		state.datagrams.push_back((datagram, now));
		state.evict_expired(now);
		Ok(())
	}

	/// Append a datagram with the next sequence number, returning the assigned sequence.
	pub fn append<B: Into<Bytes>>(&mut self, payload: B) -> Result<u64> {
		let payload = payload.into();
		if payload.len() > MAX_DATAGRAM_PAYLOAD {
			return Err(Error::WrongSize);
		}
		let mut state = self.modify()?;
		if state.closed {
			return Err(Error::Closed);
		}
		let sequence = match state.max_sequence {
			Some(s) => s.checked_add(1).ok_or(coding::BoundsExceeded)?,
			None => 0,
		};
		state.max_sequence = Some(sequence);
		let now = tokio::time::Instant::now();
		state.datagrams.push_back((Datagram { sequence, payload }, now));
		state.evict_expired(now);
		Ok(sequence)
	}

	/// Mark the channel as finished. No further datagrams may be written.
	pub fn finish(&mut self) -> Result<()> {
		let mut state = self.modify()?;
		state.closed = true;
		Ok(())
	}

	/// Abort the channel with the given error.
	pub fn abort(&mut self, err: Error) -> Result<()> {
		let mut state = self.modify()?;
		state.abort = Some(err);
		state.close();
		Ok(())
	}

	/// Create a new consumer that reads from the start of the cache.
	pub fn consume(&self) -> DatagramsConsumer {
		DatagramsConsumer {
			state: self.state.consume(),
			index: 0,
		}
	}

	/// Returns true if the channel is closed.
	pub fn is_closed(&self) -> bool {
		self.state.read().is_closed()
	}

	/// Returns true if `other` shares the same channel.
	pub fn is_clone(&self, other: &Self) -> bool {
		self.state.same_channel(&other.state)
	}

	fn modify(&self) -> Result<conducer::Mut<'_, State>> {
		self.state
			.write()
			.map_err(|r| r.abort.clone().unwrap_or(Error::Dropped))
	}
}

impl Default for DatagramsProducer {
	fn default() -> Self {
		Self::new()
	}
}

impl Clone for DatagramsProducer {
	fn clone(&self) -> Self {
		Self {
			state: self.state.clone(),
		}
	}
}

/// Reads datagrams from a shared ring.
///
/// Each consumer has its own cursor; cloning produces an independent reader.
/// `recv` / `poll_recv` take a `max_latency` parameter so the publisher's
/// per-subscription serve loop can apply the subscriber's gate without sharing
/// any extra state.
#[derive(Clone)]
pub struct DatagramsConsumer {
	state: conducer::Consumer<State>,
	index: usize,
}

impl DatagramsConsumer {
	fn poll<F, R>(&self, waiter: &conducer::Waiter, f: F) -> Poll<Result<R>>
	where
		F: Fn(&conducer::Ref<'_, State>) -> Poll<Result<R>>,
	{
		Poll::Ready(match ready!(self.state.poll(waiter, f)) {
			Ok(res) => res,
			Err(state) => Err(state.abort.clone().unwrap_or(Error::Dropped)),
		})
	}

	/// Poll for the next datagram in arrival order, filtered by `max_latency`.
	///
	/// `Duration::ZERO` disables the age filter (everything in the cache is
	/// eligible). For strict "live only" semantics, call [`Self::skip_to_latest`]
	/// once before the first poll.
	pub fn poll_recv(&mut self, waiter: &conducer::Waiter, max_latency: Duration) -> Poll<Result<Option<Datagram>>> {
		let Some((datagram, found_index)) =
			ready!(self.poll(waiter, |state| state.poll_recv(self.index, max_latency))?)
		else {
			return Poll::Ready(Ok(None));
		};
		self.index = found_index + 1;
		Poll::Ready(Ok(Some(datagram)))
	}

	/// Block until the next datagram is available (or the channel closes).
	pub async fn recv(&mut self, max_latency: Duration) -> Result<Option<Datagram>> {
		conducer::wait(|waiter| self.poll_recv(waiter, max_latency)).await
	}

	/// Skip any cached entries — start reading from the current end of the ring.
	///
	/// Useful when the consumer wants strict (`max_latency = 0`) semantics:
	/// call this once after subscribe to avoid spending a wakeup walking past
	/// stale entries on every subsequent poll.
	pub fn skip_to_latest(&mut self) {
		let state = self.state.read();
		self.index = state.offset + state.datagrams.len();
	}

	/// Poll for channel closure.
	pub fn poll_closed(&self, waiter: &conducer::Waiter) -> Poll<Result<()>> {
		self.poll(waiter, |state| state.poll_closed())
	}

	/// Block until the channel is closed.
	pub async fn closed(&self) -> Result<()> {
		conducer::wait(|waiter| self.poll_closed(waiter)).await
	}

	pub fn is_clone(&self, other: &Self) -> bool {
		self.state.same_channel(&other.state)
	}

	/// Upgrade this consumer back to a [`DatagramsProducer`] sharing the same
	/// channel. Returns `None` if the channel is already closed.
	pub fn produce(&self) -> Option<DatagramsProducer> {
		self.state.produce().map(|state| DatagramsProducer { state })
	}
}

#[cfg(test)]
mod test {
	use super::*;
	use futures::FutureExt;

	#[tokio::test]
	async fn write_and_recv() {
		let mut producer = DatagramsProducer::new();
		let mut consumer = producer.consume();

		producer
			.write(Datagram {
				sequence: 0,
				payload: Bytes::from_static(b"hello"),
			})
			.unwrap();

		let got = consumer
			.recv(Duration::from_millis(33))
			.now_or_never()
			.unwrap()
			.unwrap()
			.unwrap();
		assert_eq!(got.sequence, 0);
		assert_eq!(&got.payload[..], b"hello");
	}

	#[tokio::test]
	async fn append_auto_sequence() {
		let mut producer = DatagramsProducer::new();
		assert_eq!(producer.append(&b"a"[..]).unwrap(), 0);
		assert_eq!(producer.append(&b"b"[..]).unwrap(), 1);
		assert_eq!(producer.append(&b"c"[..]).unwrap(), 2);
	}

	#[tokio::test]
	async fn rejects_oversized_payload() {
		let mut producer = DatagramsProducer::new();
		let big = Bytes::from(vec![0u8; MAX_DATAGRAM_PAYLOAD + 1]);
		assert!(matches!(producer.append(big), Err(Error::WrongSize)));
	}

	#[tokio::test]
	async fn evicts_expired() {
		tokio::time::pause();

		let mut producer = DatagramsProducer::new();
		producer.append(&b"old"[..]).unwrap();

		tokio::time::advance(MAX_DATAGRAM_AGE + Duration::from_millis(10)).await;
		producer.append(&b"new"[..]).unwrap();

		// The eviction pass during the second append should have dropped the old entry.
		let mut consumer = producer.consume();
		let got = consumer
			.recv(Duration::from_millis(33))
			.now_or_never()
			.unwrap()
			.unwrap()
			.unwrap();
		assert_eq!(&got.payload[..], b"new");
	}

	#[tokio::test]
	async fn strict_via_skip_to_latest() {
		tokio::time::pause();

		let mut producer = DatagramsProducer::new();
		producer.append(&b"first"[..]).unwrap();

		// Strict semantics: caller skips current cache before its first poll.
		let mut strict = producer.consume();
		strict.skip_to_latest();
		assert!(strict.recv(Duration::ZERO).now_or_never().is_none());

		// A fresh arrival wakes the strict consumer.
		producer.append(&b"second"[..]).unwrap();
		let got = strict
			.recv(Duration::ZERO)
			.now_or_never()
			.expect("ready")
			.expect("ok")
			.expect("some");
		assert_eq!(&got.payload[..], b"second");

		// A separate, lax consumer can still see the original cached entry.
		let mut lax = producer.consume();
		let got = lax
			.recv(Duration::from_millis(33))
			.now_or_never()
			.expect("ready")
			.expect("ok")
			.expect("some");
		assert_eq!(&got.payload[..], b"first");
	}

	#[tokio::test]
	async fn max_latency_filters_stale_entries() {
		tokio::time::pause();

		let mut producer = DatagramsProducer::new();
		producer.append(&b"old"[..]).unwrap();

		// Age the entry past a tight latency budget but within the global cache TTL.
		tokio::time::advance(Duration::from_millis(20)).await;

		let mut consumer = producer.consume();
		// 5ms tolerance — the 20ms-old entry should be skipped.
		assert!(consumer.recv(Duration::from_millis(5)).now_or_never().is_none());

		// 30ms tolerance — the entry passes.
		let got = consumer
			.recv(Duration::from_millis(30))
			.now_or_never()
			.expect("ready")
			.expect("ok")
			.expect("some");
		assert_eq!(&got.payload[..], b"old");
	}

	#[tokio::test]
	async fn skip_to_latest_drops_history() {
		let mut producer = DatagramsProducer::new();
		producer.append(&b"old1"[..]).unwrap();
		producer.append(&b"old2"[..]).unwrap();

		let mut consumer = producer.consume();
		consumer.skip_to_latest();

		// No older datagrams visible; reading should pend.
		assert!(consumer.recv(Duration::from_millis(33)).now_or_never().is_none());

		producer.append(&b"new"[..]).unwrap();
		let got = consumer
			.recv(Duration::from_millis(33))
			.now_or_never()
			.unwrap()
			.unwrap()
			.unwrap();
		assert_eq!(&got.payload[..], b"new");
	}

	#[tokio::test]
	async fn finish_returns_none() {
		let mut producer = DatagramsProducer::new();
		let mut consumer = producer.consume();

		producer.finish().unwrap();

		let got = consumer
			.recv(Duration::from_millis(33))
			.now_or_never()
			.unwrap()
			.unwrap();
		assert!(got.is_none());
	}

	#[tokio::test]
	async fn abort_propagates() {
		let mut producer = DatagramsProducer::new();
		let mut consumer = producer.consume();

		producer.abort(Error::Cancel).unwrap();

		let res = consumer.recv(Duration::from_millis(33)).now_or_never().unwrap();
		assert!(matches!(res, Err(Error::Cancel)));
	}

	#[tokio::test]
	async fn cloned_consumer_is_independent() {
		let mut producer = DatagramsProducer::new();
		producer.append(&b"a"[..]).unwrap();
		producer.append(&b"b"[..]).unwrap();

		let mut c1 = producer.consume();
		let _ = c1.recv(Duration::from_millis(33)).now_or_never();
		let mut c2 = c1.clone();

		// c2 inherits c1's index, so it sees only "b" first.
		let got = c2
			.recv(Duration::from_millis(33))
			.now_or_never()
			.unwrap()
			.unwrap()
			.unwrap();
		assert_eq!(&got.payload[..], b"b");
	}
}
