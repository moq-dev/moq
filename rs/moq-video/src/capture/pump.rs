//! Bridges a blocking, pull-style capture device (V4L2 on Linux, Media
//! Foundation on Windows) to the async [`FrameChannel`].
//!
//! Those device reads are blocking syscalls with no async form, so they run on a
//! dedicated thread that pushes frames into the channel; the encode loop awaits
//! them like any other backend. The device is built on the thread (so a `!Send`
//! handle such as `IMFSourceReader` is fine) and dropped when the thread exits.
//! [`PumpGuard`] stops and joins the thread when the [`FrameStream`](super::FrameStream)
//! drops, releasing the device. The stop flag is checked between reads, so on a
//! live device (which delivers a frame per interval) shutdown is prompt; the join
//! is what guarantees the device fd is closed before a subsequent reopen, so we
//! don't race EBUSY. A wedged device that blocks a read forever would stall that
//! join, the same as the original `spawn_blocking` path did, but that needs a
//! driver that delivers neither a frame nor an error.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;

use super::channel::FrameChannel;
use crate::Error;
use crate::frame::Surface;

/// The negotiated geometry a backend reports once its device is open.
pub(super) struct Geometry {
	pub width: u32,
	pub height: u32,
	pub framerate: Option<u32>,
	pub device: String,
}

/// Stops and joins the pump thread on drop, releasing the device.
pub(super) struct PumpGuard {
	stop: Arc<AtomicBool>,
	handle: Option<JoinHandle<()>>,
}

impl Drop for PumpGuard {
	fn drop(&mut self) {
		self.stop.store(true, Ordering::SeqCst);
		if let Some(handle) = self.handle.take() {
			let _ = handle.join();
		}
	}
}

/// Run `init` then `read` on a dedicated thread, feeding `chan`.
///
/// `init` builds the blocking device and reports its [`Geometry`]; it runs on
/// the thread, so the device handle never has to be `Send`. `read` pulls one
/// frame per call (blocking, bounded). Returns once the device is open (or its
/// init fails), so geometry is known before the first `read().await`.
pub(super) async fn spawn<S, I, R>(
	chan: Arc<FrameChannel>,
	init: I,
	mut read: R,
) -> Result<(Geometry, PumpGuard), Error>
where
	I: FnOnce() -> Result<(S, Geometry), Error> + Send + 'static,
	R: FnMut(&mut S) -> Result<Option<Surface>, Error> + Send + 'static,
{
	let stop = Arc::new(AtomicBool::new(false));
	let (geo_tx, geo_rx) = tokio::sync::oneshot::channel();

	let handle = std::thread::spawn({
		let stop = stop.clone();
		let chan = chan.clone();
		move || {
			let (mut source, geometry) = match init() {
				Ok(opened) => opened,
				Err(err) => {
					let _ = geo_tx.send(Err(err));
					return;
				}
			};
			// If the awaiting `open` was cancelled, give up before capturing.
			if geo_tx.send(Ok(geometry)).is_err() {
				return;
			}

			while !stop.load(Ordering::SeqCst) {
				match read(&mut source) {
					Ok(Some(frame)) => chan.push(frame),
					Ok(None) => break, // device stopped producing frames
					Err(err) => {
						tracing::warn!(error = %err, "capture read failed; stopping");
						break;
					}
				}
			}

			chan.close();
			// `source` drops here, releasing the device.
		}
	});

	// Own the thread from here on, so cancelling this `await` (dropping the
	// `open` future before geometry arrives) still stops and joins it instead of
	// detaching a thread that holds the device open with the camera LED lit.
	let guard = PumpGuard {
		stop,
		handle: Some(handle),
	};

	match geo_rx.await {
		Ok(Ok(geometry)) => Ok((geometry, guard)),
		Ok(Err(err)) => Err(err),
		Err(_) => Err(Error::Codec(anyhow::anyhow!(
			"capture thread exited before reporting geometry"
		))),
	}
}
