//! A backend that records the thread each codec call ran on, so a test can pin
//! where the codec lives rather than take the platform's word for it.
//!
//! Reachable only through [`Kind::Named`](super::Kind), never through `Auto` /
//! `Hardware` / `Software`, so it can't be picked by accident.
//!
//! It also holds each frame back by one, like the Media Foundation MFT, so a
//! drain that loses the tail shows up as a missing frame rather than passing.

use std::sync::Mutex;
use std::thread::ThreadId;

use super::super::{Config, Encoded};
use super::Backend;
use crate::{Error, Frame};

pub(crate) const NAME: &str = "probe";

/// The same probe, refusing cuts: stands in for a backend whose groups fall
/// only on its own interval, so the sink's refusal can be tested without the
/// V4L2 driver that actually behaves that way.
pub(crate) const NO_CUT: &str = "probe-nocut";

/// What happened to the codec, and where. `open` and `drop` are the pair that
/// matters: the Windows backend opens a COM apartment in one and closes it in
/// the other, so they have to land on the same thread.
pub(crate) type Event = (&'static str, ThreadId);

static LOG: Mutex<Vec<Event>> = Mutex::new(Vec::new());

/// Serializes the tests that read [`LOG`], which is process-wide. nextest gives
/// each test its own process, but `cargo test` does not, and a shared log that
/// only holds up under one runner is a trap for whoever adds the next test.
static EXCLUSIVE: Mutex<()> = Mutex::new(());

/// Take the probe for one test, clearing whatever a previous one left behind.
pub(crate) fn exclusive() -> std::sync::MutexGuard<'static, ()> {
	let guard = EXCLUSIVE.lock().unwrap_or_else(|err| err.into_inner());
	let _ = take();
	guard
}

/// Held by [`hold`] to stall the codec inside [`Probe::encode`].
static GATE: Mutex<()> = Mutex::new(());

/// Stall the next encode until the returned guard drops.
///
/// Lets a test pin the codec mid-call, so a cancellation lands while the request
/// is genuinely in flight rather than racing the encode thread for it.
#[cfg(not(target_os = "macos"))]
pub(crate) fn hold() -> std::sync::MutexGuard<'static, ()> {
	GATE.lock().unwrap_or_else(|err| err.into_inner())
}

fn record(what: &'static str) {
	LOG.lock().unwrap().push((what, std::thread::current().id()));
}

/// Empty the log and hand back what was in it.
pub(crate) fn take() -> Vec<Event> {
	std::mem::take(&mut LOG.lock().unwrap())
}

pub(crate) struct Probe {
	pending: Option<Encoded>,
	cuts: bool,
}

impl Probe {
	pub(crate) fn open(_config: &Config) -> Result<Box<dyn Backend>, Error> {
		record("open");
		Ok(Box::new(Self {
			pending: None,
			cuts: true,
		}))
	}

	pub(crate) fn open_no_cut(_config: &Config) -> Result<Box<dyn Backend>, Error> {
		record("open");
		Ok(Box::new(Self {
			pending: None,
			cuts: false,
		}))
	}
}

impl Backend for Probe {
	fn encode(&mut self, frame: &Frame, cut: bool) -> Result<Vec<Encoded>, Error> {
		// Uncontended unless a test is holding the codec here on purpose.
		drop(GATE.lock().unwrap_or_else(|err| err.into_inner()));
		record("encode");
		// Recorded as its own event so a test can see the cut reach the codec on
		// the frame it was meant for.
		if cut {
			record("cut");
		}
		// The payload is the frame's timestamp, so a test can tell which frame a
		// packet came from independently of what it's stamped with.
		let payload = bytes::Bytes::from(frame.timestamp.as_micros().to_string());
		let previous = self.pending.replace(Encoded::new(payload, frame.timestamp));
		Ok(previous.into_iter().collect())
	}

	fn flush(&mut self) -> Result<Vec<Encoded>, Error> {
		record("flush");
		Ok(self.pending.take().into_iter().collect())
	}

	fn finish(&mut self) -> Result<Vec<Encoded>, Error> {
		record("finish");
		Ok(self.pending.take().into_iter().collect())
	}

	fn set_bitrate(&mut self, _bitrate: u64) -> Result<(), Error> {
		record("set_bitrate");
		Ok(())
	}

	fn can_cut(&self) -> bool {
		self.cuts
	}

	fn name(&self) -> &'static str {
		match self.cuts {
			true => NAME,
			false => NO_CUT,
		}
	}
}

impl Drop for Probe {
	fn drop(&mut self) {
		record("drop");
	}
}
