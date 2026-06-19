//! Shared macOS capture plumbing: the push (delegate thread) / pull (capture
//! thread) frame queue, and the `CMSampleBuffer` -> [`Frame::Surface`] extraction
//! used by both the AVFoundation and ScreenCaptureKit sources.

use std::collections::VecDeque;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use objc2_core_foundation::CFRetained;
use objc2_core_media::CMSampleBuffer;
use objc2_core_video::{CVImageBuffer, CVPixelBuffer, CVPixelBufferGetHeight, CVPixelBufferGetWidth};

use crate::frame::Frame;
use crate::frame::macos::Surface;

/// Bounded queue depth; older frames are dropped to favor latency.
const QUEUE_DEPTH: usize = 4;

pub(super) struct FrameQueue {
	state: Mutex<QueueState>,
	cond: Condvar,
}

struct QueueState {
	frames: VecDeque<SendFrame>,
	closed: bool,
}

/// A [`Frame`] is only `!Send` because of its `CVPixelBuffer`, which is safe to
/// move between threads (a reference-counted IOSurface wrapper). The delegate
/// produces frames on the dispatch queue; `read` consumes them on the capture
/// thread.
struct SendFrame(Frame);
unsafe impl Send for SendFrame {}

impl FrameQueue {
	pub(super) fn new() -> Arc<Self> {
		Arc::new(Self {
			state: Mutex::new(QueueState {
				frames: VecDeque::new(),
				closed: false,
			}),
			cond: Condvar::new(),
		})
	}

	pub(super) fn push(&self, frame: Frame) {
		let mut state = self.state.lock().unwrap();
		if state.frames.len() >= QUEUE_DEPTH {
			state.frames.pop_front();
		}
		state.frames.push_back(SendFrame(frame));
		self.cond.notify_one();
	}

	/// Block up to `timeout` for the next available frame.
	pub(super) fn pop_timeout(&self, timeout: Duration) -> Option<Frame> {
		let deadline = Instant::now() + timeout;
		let mut state = self.state.lock().unwrap();
		loop {
			if let Some(frame) = state.frames.pop_front() {
				return Some(frame.0);
			}
			if state.closed {
				return None;
			}
			// Honor the overall budget across spurious wakeups.
			let remaining = deadline.checked_duration_since(Instant::now())?;
			let (next, wait) = self.cond.wait_timeout(state, remaining).unwrap();
			state = next;
			if wait.timed_out() {
				return state.frames.pop_front().map(|f| f.0);
			}
		}
	}

	pub(super) fn close(&self) {
		let mut state = self.state.lock().unwrap();
		state.closed = true;
		self.cond.notify_all();
	}

	/// Whether the queue has been closed (the source is shutting down). A
	/// `None` from [`pop_timeout`](Self::pop_timeout) means closed when this is
	/// true, otherwise it was just a timeout.
	pub(super) fn is_closed(&self) -> bool {
		self.state.lock().unwrap().closed
	}
}

/// Extract the `CVPixelBuffer` from a sample buffer as a zero-copy surface.
pub(super) fn surface_frame(sample_buffer: &CMSampleBuffer) -> Option<Frame> {
	let image: CFRetained<CVImageBuffer> = unsafe { sample_buffer.image_buffer() }?;
	// CVImageBufferRef and CVPixelBufferRef are the same object for video; the
	// retain carries over with the reinterpret.
	let pixel: CFRetained<CVPixelBuffer> = unsafe { CFRetained::from_raw(CFRetained::into_raw(image).cast()) };
	let width = CVPixelBufferGetWidth(&pixel) as u32;
	let height = CVPixelBufferGetHeight(&pixel) as u32;
	Some(Frame::Surface(Surface::new(pixel, width, height)))
}

#[cfg(test)]
mod tests {
	use super::*;

	/// A timed-out `pop_timeout` on a live queue is `None` but not closed (the
	/// read maps it to `Read::Idle`); after `close` it's `None` and closed (the
	/// read maps it to `Read::End`). This is the distinction the capture loop
	/// relies on to stop without a frame ever arriving.
	#[test]
	fn pop_timeout_distinguishes_idle_from_closed() {
		let queue = FrameQueue::new();

		assert!(queue.pop_timeout(Duration::from_millis(5)).is_none());
		assert!(!queue.is_closed());

		queue.close();
		assert!(queue.pop_timeout(Duration::from_millis(5)).is_none());
		assert!(queue.is_closed());
	}
}
