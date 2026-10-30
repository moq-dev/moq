//! The waiters parked on a connection's state, woken by the changes they wait for.
//!
//! Each `poll_*` registers its caller's [`kio::Waiter`] on one of these lists, and the
//! connection wakes the list as its state changes, so a consumer keeps no waiter tables of its
//! own. A wake only schedules: the consumer polls again under its own lock.

use std::task::Poll;

use kio::{Waiter, WaiterList};
use rustc_hash::FxHashMap;

use super::Error;
use crate::{Dir, StreamId};

/// What a stream's waiter is waiting for
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Wait {
	/// Data, a FIN, or a reset to read
	Read,
	/// Room to write, or a stop
	Write,
	/// The send half ending: finished and acknowledged, stopped, or reset
	End,
	/// The send half's acknowledged prefix growing
	Acked,
}

/// The waiters on one stream
#[derive(Debug, Default)]
struct StreamWaits {
	read: WaiterList,
	write: WaiterList,
	end: WaiterList,
	acked: WaiterList,
}

impl StreamWaits {
	fn list(&mut self, wait: Wait) -> &mut WaiterList {
		match wait {
			Wait::Read => &mut self.read,
			Wait::Write => &mut self.write,
			Wait::End => &mut self.end,
			Wait::Acked => &mut self.acked,
		}
	}

	fn is_empty(&self) -> bool {
		self.read.is_empty() && self.write.is_empty() && self.end.is_empty() && self.acked.is_empty()
	}
}

/// A condition that holds once set, and the waiters for it
#[derive(Debug, Default)]
struct Latch {
	set: bool,
	waiters: WaiterList,
}

impl Latch {
	fn set(&mut self) {
		if !std::mem::replace(&mut self.set, true) {
			self.waiters.wake();
		}
	}
}

/// Every waiter on a connection, and why the connection was lost
#[derive(Debug, Default)]
pub(crate) struct Waits {
	/// Per stream, kept only while one has a live waiter or until the stream is freed
	streams: FxHashMap<StreamId, StreamWaits>,
	/// For a stream the peer opened, per direction
	accept: [WaiterList; 2],
	/// For stream credit, per direction
	open: [WaiterList; 2],
	/// For a received datagram
	datagram: WaiterList,
	connected: Latch,
	confirmed: Latch,
	/// For the connection to be lost, which every other wait also hears
	closed: WaiterList,
	/// Why the connection was lost, which fails every wait once it has drained what it can
	lost: Option<Error>,
}

impl Waits {
	/// Register `waiter` for `wait` on stream `id`
	pub(crate) fn register(&mut self, id: StreamId, wait: Wait, waiter: &Waiter) {
		waiter.register(self.streams.entry(id).or_default().list(wait));
	}

	/// Wake the waiters for `wait` on stream `id`
	pub(crate) fn wake(&mut self, id: StreamId, wait: Wait) {
		if let Some(waits) = self.streams.get_mut(&id) {
			waits.list(wait).wake();
			if waits.is_empty() {
				self.streams.remove(&id);
			}
		}
	}

	/// Wake every waiter on stream `id` and forget it, once a half of it is freed
	///
	/// Only the waits on the freed half need the wake, but a freed stream is rare enough to wake
	/// the rest too, which find their answer unchanged and register again.
	pub(crate) fn freed(&mut self, id: StreamId) {
		if let Some(mut waits) = self.streams.remove(&id) {
			for wait in [Wait::Read, Wait::Write, Wait::End, Wait::Acked] {
				waits.list(wait).wake();
			}
		}
	}

	/// Register `waiter` for a stream the peer opens in `dir`
	pub(crate) fn register_accept(&mut self, dir: Dir, waiter: &Waiter) {
		waiter.register(&mut self.accept[dir as usize]);
	}

	/// The peer opened a stream in `dir`
	pub(crate) fn wake_accept(&mut self, dir: Dir) {
		self.accept[dir as usize].wake();
	}

	/// Register `waiter` for credit to open a stream in `dir`
	pub(crate) fn register_open(&mut self, dir: Dir, waiter: &Waiter) {
		waiter.register(&mut self.open[dir as usize]);
	}

	/// The peer raised the stream limit in `dir`
	pub(crate) fn wake_open(&mut self, dir: Dir) {
		self.open[dir as usize].wake();
	}

	/// Register `waiter` for a received datagram
	pub(crate) fn register_datagram(&mut self, waiter: &Waiter) {
		waiter.register(&mut self.datagram);
	}

	/// A datagram arrived
	pub(crate) fn wake_datagram(&mut self) {
		self.datagram.wake();
	}

	/// The handshake completed
	pub(crate) fn connected(&mut self) {
		self.connected.set();
	}

	/// The handshake was confirmed
	pub(crate) fn confirmed(&mut self) {
		self.confirmed.set();
	}

	/// Ready once the handshake completed, or with the error once the connection was lost first
	pub(crate) fn poll_connected(&mut self, waiter: &Waiter) -> Poll<Result<(), Error>> {
		Self::poll_latch(&mut self.connected, &self.lost, waiter)
	}

	/// Ready once the handshake was confirmed, or with the error once the connection was lost first
	pub(crate) fn poll_confirmed(&mut self, waiter: &Waiter) -> Poll<Result<(), Error>> {
		Self::poll_latch(&mut self.confirmed, &self.lost, waiter)
	}

	fn poll_latch(latch: &mut Latch, lost: &Option<Error>, waiter: &Waiter) -> Poll<Result<(), Error>> {
		if latch.set {
			return Poll::Ready(Ok(()));
		}
		if let Some(err) = lost {
			return Poll::Ready(Err(err.clone()));
		}
		waiter.register(&mut latch.waiters);
		Poll::Pending
	}

	/// Ready with the reason once the connection is lost
	pub(crate) fn poll_closed(&mut self, waiter: &Waiter) -> Poll<Error> {
		match &self.lost {
			Some(err) => Poll::Ready(err.clone()),
			None => {
				waiter.register(&mut self.closed);
				Poll::Pending
			}
		}
	}

	/// Why the connection was lost, if it was
	pub(crate) fn lost(&self) -> Option<&Error> {
		self.lost.as_ref()
	}

	/// The connection was lost, so wake every waiter to drain what it can and fail
	///
	/// The first reason sticks.
	pub(crate) fn lose(&mut self, reason: Error) {
		if self.lost.is_some() {
			return;
		}
		self.lost = Some(reason);
		for (_, mut waits) in self.streams.drain() {
			for wait in [Wait::Read, Wait::Write, Wait::End, Wait::Acked] {
				waits.list(wait).wake();
			}
		}
		for list in self.accept.iter_mut().chain(&mut self.open) {
			list.wake();
		}
		self.datagram.wake();
		self.connected.waiters.wake();
		self.confirmed.waiters.wake();
		self.closed.wake();
	}

	/// The streams with a waiter entry, to prove the map stays bounded
	#[cfg(test)]
	pub(crate) fn stream_entries(&self) -> usize {
		self.streams.len()
	}
}

/// A waiter that counts its wakes, so a test can assert who heard a change
#[cfg(test)]
pub(crate) struct Counter {
	count: std::sync::Arc<Count>,
	waiter: Waiter,
}

#[cfg(test)]
#[derive(Default)]
struct Count(std::sync::atomic::AtomicUsize);

#[cfg(test)]
impl std::task::Wake for Count {
	fn wake(self: std::sync::Arc<Self>) {
		self.wake_by_ref();
	}

	fn wake_by_ref(self: &std::sync::Arc<Self>) {
		self.0.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
	}
}

#[cfg(test)]
impl Counter {
	pub(crate) fn new() -> Self {
		let count = std::sync::Arc::new(Count::default());
		let waiter = Waiter::new(std::task::Waker::from(count.clone()));
		Self { count, waiter }
	}

	/// How many times the waiter was woken
	pub(crate) fn wakes(&self) -> usize {
		self.count.0.load(std::sync::atomic::Ordering::Relaxed)
	}

	/// The count alone, which outlives the waiter
	pub(crate) fn count(&self) -> impl Fn() -> usize + use<> {
		let count = self.count.clone();
		move || count.0.load(std::sync::atomic::Ordering::Relaxed)
	}
}

#[cfg(test)]
impl std::ops::Deref for Counter {
	type Target = Waiter;

	fn deref(&self) -> &Waiter {
		&self.waiter
	}
}
