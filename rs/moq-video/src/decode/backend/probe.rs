//! A decoder that records the thread each call ran on, so a test can pin where
//! the codec lives rather than take the platform's word for it.
//!
//! The decode-side twin of [`encode::backend::probe`](crate::encode). Separate
//! statics rather than a shared log, so the two can be exercised in one process
//! without either having to know about the other.
//!
//! Reachable only through [`Kind::Named`](super::Kind), never through `Auto` /
//! `Hardware` / `Software`, so it can't be picked by accident.

use std::sync::Mutex;
#[cfg(not(target_os = "macos"))]
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::ThreadId;

use bytes::Bytes;
use moq_net::Timestamp;

use super::{Backend, Codec, Config};
use crate::{Error, Frame, I420, Size, Surface};

pub(crate) const NAME: &str = "probe";

/// A test decoder that holds one picture until the next call or a flush.
pub(crate) const BUFFERED_NAME: &str = "probe-buffered";

/// A test decoder whose flush waits until the test releases it.
#[cfg(not(target_os = "macos"))]
pub(crate) const BLOCKING_FLUSH_NAME: &str = "probe-blocking-flush";

/// What happened to the codec, and where. `open` and `drop` are the pair that
/// matters: the Windows backend opens a COM apartment in one and closes it in
/// the other, so they have to land on the same thread.
pub(crate) type Event = (&'static str, ThreadId);

static LOG: Mutex<Vec<Event>> = Mutex::new(Vec::new());

#[cfg(not(target_os = "macos"))]
static FLUSH_ENTERED: AtomicBool = AtomicBool::new(false);

#[cfg(not(target_os = "macos"))]
static FLUSH_RELEASED: AtomicBool = AtomicBool::new(false);

/// Serializes the tests that read [`LOG`], which is process-wide. nextest gives
/// each test its own process, but `cargo test` does not.
#[cfg(not(target_os = "macos"))]
static EXCLUSIVE: Mutex<()> = Mutex::new(());

/// Take the probe for one test, clearing whatever a previous one left behind.
#[cfg(not(target_os = "macos"))]
pub(crate) fn exclusive() -> std::sync::MutexGuard<'static, ()> {
	let guard = EXCLUSIVE.lock().unwrap_or_else(|err| err.into_inner());
	let _ = take();
	guard
}

/// Empty the log and hand back what was in it.
#[cfg(not(target_os = "macos"))]
pub(crate) fn take() -> Vec<Event> {
	std::mem::take(&mut LOG.lock().unwrap())
}

/// Prepare the blocking flush probe for one cancellation test.
#[cfg(not(target_os = "macos"))]
pub(crate) fn prepare_blocking_flush() {
	FLUSH_ENTERED.store(false, Ordering::SeqCst);
	FLUSH_RELEASED.store(false, Ordering::SeqCst);
}

/// Whether the blocking flush has started on the codec thread.
#[cfg(not(target_os = "macos"))]
pub(crate) fn flush_entered() -> bool {
	FLUSH_ENTERED.load(Ordering::SeqCst)
}

/// Let the blocking flush finish so the codec thread can be joined.
#[cfg(not(target_os = "macos"))]
pub(crate) fn release_flush() {
	FLUSH_RELEASED.store(true, Ordering::SeqCst);
}

fn record(what: &'static str) {
	LOG.lock().unwrap().push((what, std::thread::current().id()));
}

/// The size the probe claims to decode at, so a test can build frames without a
/// real bitstream.
pub(crate) const SIZE: Size = Size {
	width: 320,
	height: 240,
};

pub(crate) struct Probe;

pub(crate) struct Buffered(Option<Frame>);

#[cfg(not(target_os = "macos"))]
pub(crate) struct BlockingFlush;

impl Probe {
	pub(crate) fn open(_codec: Codec, _config: &Config) -> Result<Box<dyn Backend>, Error> {
		record("open");
		Ok(Box::new(Self))
	}
}

impl Buffered {
	pub(crate) fn open(_codec: Codec, _config: &Config) -> Result<Box<dyn Backend>, Error> {
		Ok(Box::new(Self(None)))
	}
}

#[cfg(not(target_os = "macos"))]
impl BlockingFlush {
	pub(crate) fn open(_codec: Codec, _config: &Config) -> Result<Box<dyn Backend>, Error> {
		Ok(Box::new(Self))
	}
}

fn frame(timestamp: Timestamp) -> Result<Frame, Error> {
	let i420 = I420::new(
		SIZE.width,
		SIZE.height,
		vec![0x80u8; I420::len(SIZE.width, SIZE.height)],
	)?;
	Ok(Frame::new(Surface::I420(i420), timestamp))
}

impl Backend for Probe {
	/// One mid-gray frame per access unit, carrying the timestamp it came in
	/// with, so a test can tell which payload a frame came from.
	fn decode(&mut self, _access_unit: Bytes, timestamp: Timestamp, _keyframe: bool) -> Result<Vec<Frame>, Error> {
		record("decode");
		Ok(vec![frame(timestamp)?])
	}

	fn flush(&mut self) -> Result<Vec<Frame>, Error> {
		Ok(Vec::new())
	}

	fn name(&self) -> &str {
		NAME
	}
}

#[cfg(not(target_os = "macos"))]
impl Backend for BlockingFlush {
	fn decode(&mut self, _access_unit: Bytes, _timestamp: Timestamp, _keyframe: bool) -> Result<Vec<Frame>, Error> {
		Ok(Vec::new())
	}

	fn flush(&mut self) -> Result<Vec<Frame>, Error> {
		FLUSH_ENTERED.store(true, Ordering::SeqCst);
		while !FLUSH_RELEASED.load(Ordering::SeqCst) {
			std::thread::yield_now();
		}
		Ok(Vec::new())
	}

	fn name(&self) -> &str {
		BLOCKING_FLUSH_NAME
	}
}

impl Backend for Buffered {
	fn decode(&mut self, _access_unit: Bytes, timestamp: Timestamp, _keyframe: bool) -> Result<Vec<Frame>, Error> {
		Ok(self.0.replace(frame(timestamp)?).into_iter().collect())
	}

	fn flush(&mut self) -> Result<Vec<Frame>, Error> {
		Ok(self.0.take().into_iter().collect())
	}

	fn name(&self) -> &str {
		BUFFERED_NAME
	}
}

impl Drop for Probe {
	fn drop(&mut self) {
		record("drop");
	}
}
