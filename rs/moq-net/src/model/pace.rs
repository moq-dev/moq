//! Fastest-reader pacing: how far a group's writer reads ahead of its furthest reader.
//!
//! Readers report the frames they take; a paced writer (a session's ingest) stops reading
//! its upstream stream while it is more than [`AHEAD`] past the end of the last frame the
//! fastest reader took, so stream flow control pushes back to the publisher. Only the
//! fastest reader counts: a slower one is never waited on, and falls off the group's
//! front as the live tail sheds.
//!
//! Progress is in bytes on one scale per group copy: each frame entering the copy's log
//! ends where the frames before it, payload and slot, add up to. A frame larger than the
//! window never stalls its writer, since a reader that took every committed frame is
//! waiting on the writer.

use std::sync::{
	Mutex, PoisonError,
	atomic::{AtomicBool, AtomicU64, Ordering},
};
use std::task::Waker;

use super::group::MAX_CACHE_BYTES;

/// How far a paced writer runs ahead of its group's fastest reader: half the live tail,
/// so the other half stays behind the fastest reader for slower ones.
pub(super) const AHEAD: u64 = MAX_CACHE_BYTES / 2;

/// One group copy's reader progress, shared by its writer and every cursor.
#[derive(Default)]
pub(super) struct Pace {
	// Where the last frame the fastest reader took ends.
	read: AtomicU64,
	// Where the last committed frame ends.
	written: AtomicU64,
	// Where the last frame shed from the copy's front ends: a reader short of it fell off.
	front: AtomicU64,
	// Set while the writer waits on `read`, so a read wakes it without locking otherwise.
	parked: AtomicBool,
	// The writer's waker: a group has one writer, so one slot, far smaller than a list.
	writer: Mutex<Option<Waker>>,
}

impl Pace {
	/// A reader took the frame ending at `end`, and every frame before it.
	pub fn took(&self, end: u64) {
		// The common case, a reader trailing the fastest one, writes nothing.
		if self.read.load(Ordering::Relaxed) >= end {
			return;
		}
		// SeqCst pairs with `park`: either this sees the writer parked, or it sees the read.
		self.read.fetch_max(end, Ordering::SeqCst);
		if self.parked.swap(false, Ordering::SeqCst) {
			let writer = self.writer.lock().unwrap_or_else(PoisonError::into_inner).take();
			if let Some(writer) = writer {
				writer.wake();
			}
		}
	}

	/// A reader took every frame committed so far, such as by taking the open one.
	pub fn took_committed(&self) {
		self.took(self.written());
	}

	/// Count a committed frame costing `cost`, returning where it ends.
	///
	/// Called under the group's lock, so commits never race.
	pub fn commit(&self, cost: u64) -> u64 {
		let end = self.written() + cost;
		self.written.store(end, Ordering::Release);
		end
	}

	/// The copy shed every frame up to the one ending at `end`.
	pub fn shed(&self, end: u64) {
		self.front.fetch_max(end, Ordering::Release);
	}

	fn written(&self) -> u64 {
		self.written.load(Ordering::Acquire)
	}

	/// Whether a writer `partial` bytes into an uncommitted frame is more than [`AHEAD`]
	/// past the fastest reader, given it read up to `read`.
	///
	/// Never while that reader took every committed frame, since it is waiting on the
	/// writer, nor once the frames it has yet to take were shed, since it fell off.
	pub fn paced(&self, read: u64, partial: u64) -> bool {
		let written = self.written();
		read < written && read >= self.front.load(Ordering::Acquire) && written.saturating_add(partial) - read > AHEAD
	}

	/// Whether the writer is paced, as [`Self::paced`] judges it now.
	pub fn ahead(&self, partial: u64) -> bool {
		self.paced(self.read.load(Ordering::SeqCst), partial)
	}

	/// Register `waiter` for the next read, returning how far readers got, read after
	/// registering so a read in between is never missed.
	pub fn park(&self, waiter: &kio::Waiter) -> u64 {
		let mut writer = self.writer.lock().unwrap_or_else(PoisonError::into_inner);
		match &mut *writer {
			Some(waker) => waker.clone_from(waiter.waker()),
			None => *writer = Some(waiter.waker().clone()),
		}
		drop(writer);
		self.parked.store(true, Ordering::SeqCst);
		self.read.load(Ordering::SeqCst)
	}
}

#[cfg(test)]
mod tests {
	use std::sync::Arc;
	use std::task::Poll;

	use bytes::Bytes;

	use super::*;
	use crate::{Error, frame, group};

	const MB: usize = 1 << 20;

	struct Flag(AtomicBool);

	impl std::task::Wake for Flag {
		fn wake(self: Arc<Self>) {
			self.0.store(true, Ordering::SeqCst);
		}
	}

	impl Flag {
		fn woken(&self) -> bool {
			self.0.swap(false, Ordering::SeqCst)
		}
	}

	fn flag() -> (Arc<Flag>, kio::Waiter) {
		let flag = Arc::new(Flag(AtomicBool::new(false)));
		let waiter = kio::Waiter::new(std::task::Waker::from(flag.clone()));
		(flag, waiter)
	}

	/// An untimed group with `frames` frames of a megabyte each.
	fn written(frames: usize) -> group::Producer {
		let mut group = group::Info { sequence: 0 }.produce_untimed();
		let payload = Bytes::from(vec![0u8; MB]);
		for _ in 0..frames {
			group.write_frame(None, payload.clone()).unwrap();
		}
		group
	}

	fn read(reader: &mut group::Consumer) {
		let Poll::Ready(Ok(Some(_))) = reader.poll_read_frame(&kio::Waiter::noop()) else {
			panic!("a committed frame is ready");
		};
	}

	/// A writer more than the window ahead of its fastest reader waits, and that reader
	/// taking frames lets it on; a slower reader is never waited on.
	#[test]
	fn the_fastest_reader_paces_the_writer() {
		let (flag, waiter) = flag();
		let mut group = written(15);
		let mut fast = group.consume();
		let _slow = group.consume();
		assert_eq!(group.poll_ready(&waiter, 0), Poll::Ready(()), "within the window");

		group.write_frame(None, Bytes::from(vec![0u8; MB])).unwrap();
		assert_eq!(group.poll_ready(&waiter, 0), Poll::Pending, "past the window");
		assert!(!flag.woken());

		// A batch read takes several frames at once, which the writer counts as read.
		read(&mut fast);
		assert!(flag.woken(), "the fastest reader's read wakes the writer");
		assert_eq!(group.poll_ready(&waiter, 0), Poll::Ready(()));
	}

	/// Bytes measure the window, not frames, so an open frame counts as it fills.
	#[test]
	fn an_open_frame_counts_toward_the_window() {
		let (_flag, waiter) = flag();
		let group = written(15);
		let _reader = group.consume();
		let mut frame = group
			.create_frame(frame::Info {
				size: 2 * MB as u64,
				timestamp: None,
			})
			.unwrap();
		assert_eq!(frame.poll_ready(&waiter), Poll::Ready(()));
		frame.write(Bytes::from(vec![0u8; MB])).unwrap();
		assert_eq!(frame.poll_ready(&waiter), Poll::Pending);
	}

	/// A group nobody reads keeps writing, and a parked writer moves on once its last
	/// reader leaves.
	#[test]
	fn a_group_without_readers_never_waits() {
		let (flag, waiter) = flag();
		let group = written(16);
		assert_eq!(group.poll_ready(&waiter, 0), Poll::Ready(()));

		let reader = group.consume();
		assert_eq!(
			group.poll_ready(&waiter, 0),
			Poll::Pending,
			"a reader that took nothing"
		);
		drop(reader);
		assert!(flag.woken(), "the last reader leaving wakes the writer");
		assert_eq!(group.poll_ready(&waiter, 0), Poll::Ready(()));
	}

	/// A reader that took every committed frame is waiting on the writer, so a frame
	/// larger than the window never stalls against it.
	#[test]
	fn a_reader_at_the_edge_never_stalls_a_large_frame() {
		let (_flag, waiter) = flag();
		let group = written(1);
		let mut reader = group.consume();
		read(&mut reader);
		assert!(reader.poll_read_frame(&kio::Waiter::noop()).is_pending());

		let mut frame = group
			.create_frame(frame::Info {
				size: 20 * MB as u64,
				timestamp: None,
			})
			.unwrap();
		frame.write(Bytes::from(vec![0u8; 18 * MB])).unwrap();
		assert_eq!(frame.poll_ready(&waiter), Poll::Ready(()));
	}

	/// A reader whose next frame the tail shed fell off the group, so the writer stops
	/// waiting on it.
	#[test]
	fn a_reader_that_fell_off_is_not_waited_on() {
		let (_flag, waiter) = flag();
		let mut group = written(1);
		let _reader = group.consume();
		// A local writer never waits, so it sheds the front past the 32 MB tail.
		for _ in 0..40 {
			group.write_frame(None, Bytes::from(vec![0u8; MB])).unwrap();
		}
		assert_eq!(group.poll_ready(&waiter, 0), Poll::Ready(()));
	}

	/// An abort wakes a parked writer, so its next write surfaces why.
	#[test]
	fn an_abort_wakes_a_parked_writer() {
		let (flag, waiter) = flag();
		let group = written(17);
		let _reader = group.consume();
		assert_eq!(group.poll_ready(&waiter, 0), Poll::Pending);
		group.handle().abort(Error::Cancel).unwrap();
		assert!(flag.woken());
		assert_eq!(group.poll_ready(&waiter, 0), Poll::Ready(()));
	}
}
