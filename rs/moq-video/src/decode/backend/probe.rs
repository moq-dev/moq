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
use std::thread::ThreadId;

use bytes::Bytes;
use moq_net::Timestamp;

use super::{Backend, Codec, Config};
use crate::{Error, Frame, I420, Size, Surface};

pub(crate) const NAME: &str = "probe";

/// What happened to the codec, and where. `open` and `drop` are the pair that
/// matters: the Windows backend opens a COM apartment in one and closes it in
/// the other, so they have to land on the same thread.
pub(crate) type Event = (&'static str, ThreadId);

static LOG: Mutex<Vec<Event>> = Mutex::new(Vec::new());

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

impl Probe {
	pub(crate) fn open(_codec: Codec, _config: &Config) -> Result<Box<dyn Backend>, Error> {
		record("open");
		Ok(Box::new(Self))
	}
}

impl Backend for Probe {
	/// One mid-gray frame per access unit, carrying the timestamp it came in
	/// with, so a test can tell which payload a frame came from.
	fn decode(&mut self, _access_unit: Bytes, timestamp: Timestamp, _keyframe: bool) -> Result<Vec<Frame>, Error> {
		record("decode");
		let i420 = I420::new(
			SIZE.width,
			SIZE.height,
			vec![0x80u8; I420::len(SIZE.width, SIZE.height)],
		)?;
		Ok(vec![Frame::new(Surface::I420(i420), timestamp)])
	}

	fn name(&self) -> &str {
		NAME
	}
}

impl Drop for Probe {
	fn drop(&mut self) {
		record("drop");
	}
}
