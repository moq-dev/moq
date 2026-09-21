//! An async, latest-frame channel shared by every capture backend.
//!
//! Backends produce frames from a foreign thread (the macOS delegate dispatch
//! queue, or the V4L2 / Media Foundation pump thread) via the synchronous
//! [`push`](FrameChannel::push); the encode loop consumes them with the async
//! [`recv`](FrameChannel::recv). Because `recv` is a real `.await`, dropping the
//! capture future cancels it promptly, which is what makes capture cancel-safe:
//! the [`Stream`](super::Stream) drops, the device is released, and no
//! blocking thread is left pinned.

use std::sync::{Arc, Mutex};
use std::time::Instant;

use tokio::sync::Notify;

use crate::Error;
use crate::frame::{Frame, Surface};
use moq_net::Timestamp;

/// The producer/consumer rendezvous for a single capture session.
pub(super) struct FrameChannel {
	state: Mutex<State>,
	notify: Notify,
	epoch: Instant,
}

struct State {
	frame: Option<Frame>,
	native_anchor: Option<(Timestamp, Timestamp)>,
	closed: bool,
	error: Option<Error>,
}

impl FrameChannel {
	pub(super) fn new() -> Arc<Self> {
		Arc::new(Self {
			state: Mutex::new(State {
				frame: None,
				native_anchor: None,
				closed: false,
				error: None,
			}),
			notify: Notify::new(),
			epoch: Instant::now(),
		})
	}

	/// Publish the latest frame, replacing one the consumer has not reached. Safe
	/// to call from the foreign producer thread; a no-op once closed.
	pub(super) fn push(&self, frame: Surface) {
		self.push_at(frame, Instant::now());
	}

	fn push_at(&self, surface: Surface, captured: Instant) {
		let micros = captured.saturating_duration_since(self.epoch).as_micros();
		let micros = u64::try_from(micros).unwrap_or(u64::MAX);
		let frame = Frame::new(surface, Timestamp::from_micros(micros).expect("capture timestamp fits"));
		self.publish(frame);
	}

	/// Map a device-local timestamp into this stream's private timeline. The
	/// source epoch never escapes: its first sample is anchored to acquisition.
	pub(super) fn push_native(&self, surface: Surface, source: Timestamp) {
		let local = self.now();
		let mut state = self.state.lock().unwrap();
		if state.closed {
			return;
		}
		let (source_anchor, local_anchor) = *state.native_anchor.get_or_insert((source, local));
		let timestamp = source
			.checked_sub(source_anchor)
			.and_then(|elapsed| local_anchor.checked_add(elapsed))
			.unwrap_or(local);
		state.frame = Some(Frame::new(surface, timestamp));
		drop(state);
		self.notify.notify_one();
	}

	fn publish(&self, frame: Frame) {
		let mut state = self.state.lock().unwrap();
		if state.closed {
			return;
		}
		state.frame = Some(frame);
		drop(state);
		self.notify.notify_one();
	}

	/// Mark the source ended, so a parked [`recv`](Self::recv) returns `None`.
	pub(super) fn close(&self) {
		let mut state = self.state.lock().unwrap();
		state.closed = true;
		drop(state);
		self.wake();
	}

	/// End the source with an error. Any pending frame is discarded so source
	/// removal or revoked permission reaches the consumer immediately.
	pub(super) fn fail(&self, error: Error) {
		let mut state = self.state.lock().unwrap();
		if state.closed {
			return;
		}
		state.frame = None;
		state.error = Some(error);
		state.closed = true;
		drop(state);
		self.wake();
	}

	/// Wake the single consumer, retaining a permit if it has not parked yet.
	///
	/// `notify_waiters` would drop the wakeup on the floor here: `recv` builds
	/// its `Notified` before taking the lock but only registers it when the
	/// future is first polled, so a terminal state set from the producer thread
	/// in that window would leave the consumer asleep forever.
	fn wake(&self) {
		self.notify.notify_one();
	}

	/// Await the latest frame, the terminal backend error, or `None` once closed.
	pub(super) async fn recv(&self) -> Result<Option<Frame>, Error> {
		loop {
			// Register for a wakeup before checking, so a `push` that races the
			// check still wakes this future (tokio's documented Notify pattern).
			let notified = self.notify.notified();
			{
				let mut state = self.state.lock().unwrap();
				if let Some(error) = state.error.take() {
					return Err(error);
				}
				if let Some(frame) = state.frame.take() {
					return Ok(Some(frame));
				}
				if state.closed {
					return Ok(None);
				}
			}
			notified.await;
		}
	}

	pub(super) fn now(&self) -> Timestamp {
		let micros = u64::try_from(self.epoch.elapsed().as_micros()).unwrap_or(u64::MAX);
		Timestamp::from_micros(micros).expect("capture timestamp fits")
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
			color: None,
		})
	}

	#[tokio::test]
	async fn recv_returns_frames_in_order() {
		let chan = FrameChannel::new();
		chan.push(frame(1));
		assert_eq!(chan.recv().await.unwrap().unwrap().surface.width(), 1);
		chan.push(frame(2));
		assert_eq!(chan.recv().await.unwrap().unwrap().surface.width(), 2);
	}

	#[tokio::test]
	async fn slow_consumer_receives_only_the_latest_frame() {
		let chan = FrameChannel::new();
		for id in 1..=6 {
			chan.push(frame(id));
		}
		assert_eq!(chan.recv().await.unwrap().unwrap().surface.width(), 6);
	}

	#[tokio::test]
	async fn close_returns_none_after_the_pending_frame() {
		let chan = FrameChannel::new();
		chan.push(frame(1));
		chan.close();
		assert_eq!(chan.recv().await.unwrap().unwrap().surface.width(), 1);
		assert!(chan.recv().await.unwrap().is_none());
	}

	#[tokio::test]
	async fn failure_discards_a_pending_frame_and_surfaces_the_cause() {
		let chan = FrameChannel::new();
		chan.push(frame(1));
		chan.fail(Error::SourceUnavailable("window closed".to_string()));

		assert!(matches!(
			chan.recv().await,
			Err(Error::SourceUnavailable(reason)) if reason == "window closed"
		));
		assert!(chan.recv().await.unwrap().is_none());
	}

	/// A terminal state set while the consumer is between building its `Notified`
	/// and registering it must still wake that consumer, so the wakeup has to
	/// leave a permit behind rather than only signalling registered waiters.
	#[tokio::test]
	async fn closing_retains_a_wakeup_for_a_consumer_that_has_not_parked() {
		let chan = FrameChannel::new();
		// A permit consumed by nobody stands in for the unregistered consumer.
		chan.close();
		chan.notify.notified().await;

		let chan = FrameChannel::new();
		chan.fail(Error::SourceUnavailable("stream stopped".to_string()));
		chan.notify.notified().await;
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
		assert_eq!(chan.recv().await.unwrap().unwrap().surface.width(), 7);
	}

	#[tokio::test]
	async fn timestamp_is_captured_before_queued_delay() {
		let chan = FrameChannel::new();
		let captured = chan.epoch + std::time::Duration::from_millis(12);
		chan.push_at(frame(1), captured);
		assert_eq!(chan.recv().await.unwrap().unwrap().timestamp.as_micros(), 12_000);
	}

	#[tokio::test]
	async fn native_timestamps_keep_deltas_without_exposing_the_device_epoch() {
		let chan = FrameChannel::new();
		let first_source = Timestamp::from_micros(9_000_000).unwrap();
		chan.push_native(frame(1), first_source);
		let first = chan.recv().await.unwrap().unwrap().timestamp;
		chan.push_native(frame(2), Timestamp::from_micros(9_033_367).unwrap());
		let second = chan.recv().await.unwrap().unwrap().timestamp;
		assert_eq!(second.as_micros() - first.as_micros(), 33_367);
		assert!(first.as_micros() < 9_000_000);
	}
}
