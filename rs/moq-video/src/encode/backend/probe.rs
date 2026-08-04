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

/// What happened to the codec, and where. `open` and `drop` are the pair that
/// matters: the Windows backend opens a COM apartment in one and closes it in
/// the other, so they have to land on the same thread.
pub(crate) type Event = (&'static str, ThreadId);

static LOG: Mutex<Vec<Event>> = Mutex::new(Vec::new());

fn record(what: &'static str) {
	LOG.lock().unwrap().push((what, std::thread::current().id()));
}

/// Empty the log and hand back what was in it.
pub(crate) fn take() -> Vec<Event> {
	std::mem::take(&mut LOG.lock().unwrap())
}

pub(crate) struct Probe {
	pending: Option<Encoded>,
}

impl Probe {
	pub(crate) fn open(_config: &Config) -> Result<Box<dyn Backend>, Error> {
		record("open");
		Ok(Box::new(Self { pending: None }))
	}
}

impl Backend for Probe {
	fn encode(&mut self, frame: &Frame, _keyframe: bool) -> Result<Vec<Encoded>, Error> {
		record("encode");
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

	fn name(&self) -> &str {
		NAME
	}
}

impl Drop for Probe {
	fn drop(&mut self) {
		record("drop");
	}
}
