//! An async, bounded frame channel shared by every capture backend.
//!
//! Backends produce frames from a foreign thread (the macOS delegate dispatch
//! queue, or the V4L2 / Media Foundation pump thread) via the synchronous
//! [`push`](FrameChannel::push); the encode loop consumes them with the async
//! [`recv`](FrameChannel::recv). Because `recv` is a real `.await`, dropping the
//! capture future cancels it promptly, which is what makes capture cancel-safe:
//! the [`FrameStream`](super::FrameStream) drops, the device is released, and no
//! blocking thread is left pinned.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use tokio::sync::Notify;

use crate::frame::Surface;

/// Bounded depth; the oldest frame is dropped to favor latency over completeness.
const DEPTH: usize = 4;

/// The producer/consumer rendezvous for a single capture session.
pub(super) struct FrameChannel {
	state: Mutex<State>,
	notify: Notify,
}

struct State {
	frames: VecDeque<Surface>,
	closed: bool,
}

impl FrameChannel {
	pub(super) fn new() -> Arc<Self> {
		Arc::new(Self {
			state: Mutex::new(State {
				frames: VecDeque::new(),
				closed: false,
			}),
			notify: Notify::new(),
		})
	}

	/// Enqueue a frame, dropping the oldest if the buffer is full. Safe to call
	/// from the foreign producer thread; a no-op once closed.
	pub(super) fn push(&self, frame: Surface) {
		{
			let mut state = self.state.lock().unwrap();
			if state.closed {
				return;
			}
			if state.frames.len() >= DEPTH {
				state.frames.pop_front();
			}
			state.frames.push_back(frame);
		}
		self.notify.notify_one();
	}

	/// Mark the source ended, so a parked [`recv`](Self::recv) returns `None`.
	pub(super) fn close(&self) {
		self.state.lock().unwrap().closed = true;
		self.notify.notify_waiters();
	}

	/// Await the next frame, or `None` once the channel is closed and drained.
	pub(super) async fn recv(&self) -> Option<Surface> {
		loop {
			// Register for a wakeup before checking, so a `push` that races the
			// check still wakes this future (tokio's documented Notify pattern).
			let notified = self.notify.notified();
			{
				let mut state = self.state.lock().unwrap();
				if let Some(frame) = state.frames.pop_front() {
					return Some(frame);
				}
				if state.closed {
					return None;
				}
			}
			notified.await;
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::frame::I420;

	/// A throwaway frame tagged via its width, so a test can identify which frame
	/// `recv` returned without building real pixel data.
	fn frame(id: u32) -> Surface {
		Surface::I420(I420 {
			width: id,
			height: 2,
			data: Vec::new(),
		})
	}

	#[tokio::test]
	async fn recv_returns_frames_in_order() {
		let chan = FrameChannel::new();
		chan.push(frame(1));
		chan.push(frame(2));
		assert_eq!(chan.recv().await.unwrap().width(), 1);
		assert_eq!(chan.recv().await.unwrap().width(), 2);
	}

	#[tokio::test]
	async fn drops_oldest_when_full() {
		let chan = FrameChannel::new();
		// DEPTH + 2 frames pushed: only the newest DEPTH survive, favoring latency.
		for id in 1..=(DEPTH as u32 + 2) {
			chan.push(frame(id));
		}
		for id in 3..=(DEPTH as u32 + 2) {
			assert_eq!(chan.recv().await.unwrap().width(), id);
		}
	}

	#[tokio::test]
	async fn close_returns_none_after_draining() {
		let chan = FrameChannel::new();
		chan.push(frame(1));
		chan.close();
		// Buffered frames drain first, then `None` signals the source ended.
		assert_eq!(chan.recv().await.unwrap().width(), 1);
		assert!(chan.recv().await.is_none());
	}

	/// Cancelling a parked `recv` (as the encode loop's `select!` does each time a
	/// frame loses the race) must not drop a wakeup: a later `recv` still sees the
	/// next frame. Frames live in the queue, not the notification, so this holds.
	#[tokio::test]
	async fn recv_is_cancel_safe() {
		let chan = FrameChannel::new();
		// Poll `recv` to Pending (registering its waker), then cancel it.
		tokio::select! {
			_ = chan.recv() => panic!("no frame pushed yet"),
			_ = std::future::ready(()) => {}
		}
		chan.push(frame(7));
		assert_eq!(chan.recv().await.unwrap().width(), 7);
	}
}
