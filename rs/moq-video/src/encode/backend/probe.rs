//! A backend that records the thread each codec call ran on, so a test can pin
//! where the codec lives rather than take the platform's word for it.
//!
//! Reachable only through [`Kind::Named`](super::Kind), never through `Auto` /
//! `Hardware` / `Software`, so it can't be picked by accident.
//!
//! It also holds each frame back by one, like the Media Foundation MFT, so a
//! drain that loses the tail shows up as a missing frame rather than passing.
//! Every bitrate it accepts is logged too, so a test can follow rate control.

use std::collections::VecDeque;
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

/// OpenH264 held back by one access unit, like Media Foundation and MediaCodec:
/// a real bitstream a track can publish, whose last frame only a flush returns.
#[cfg(feature = "openh264")]
pub(crate) const PIPELINED: &str = "probe-pipelined";

/// What happened to the codec, and where. `open` and `drop` are the pair that
/// matters: the Windows backend opens a COM apartment in one and closes it in
/// the other, so they have to land on the same thread.
pub(crate) type Event = (&'static str, ThreadId);

static LOG: Mutex<Vec<Event>> = Mutex::new(Vec::new());

/// Every bitrate the probe accepted, in order.
static BITRATES: Mutex<Vec<u64>> = Mutex::new(Vec::new());

/// A bitrate the probe refuses once, standing in for a transient codec failure.
static REFUSE: Mutex<Option<u64>> = Mutex::new(None);

/// How many of the next opens fail, standing in for a codec that is busy or gone.
static FAIL_OPENS: Mutex<u32> = Mutex::new(0);

/// Whether the next flush fails, standing in for a codec that loses its tail.
static LOSE_TAIL: Mutex<bool> = Mutex::new(false);

/// Serializes the tests that read [`LOG`], which is process-wide. nextest gives
/// each test its own process, but `cargo test` does not, and a shared log that
/// only holds up under one runner is a trap for whoever adds the next test.
static EXCLUSIVE: Mutex<()> = Mutex::new(());

/// Take the probe for one test, clearing whatever a previous one left behind.
pub(crate) fn exclusive() -> std::sync::MutexGuard<'static, ()> {
	let guard = EXCLUSIVE.lock().unwrap_or_else(|err| err.into_inner());
	let _ = take();
	BITRATES.lock().unwrap().clear();
	REFUSE.lock().unwrap().take();
	*FAIL_OPENS.lock().unwrap() = 0;
	*LOSE_TAIL.lock().unwrap() = false;
	guard
}

/// Held by [`hold`] to stall the codec inside [`Probe::encode`].
static GATE: Mutex<()> = Mutex::new(());

/// Stall the next encode until the returned guard drops.
///
/// Lets a test pin the codec mid-call, so a cancellation lands while the request
/// is genuinely in flight rather than racing the encode thread for it.
#[cfg(not(apple))]
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

/// Empty the accepted bitrates and hand them back.
pub(crate) fn bitrates() -> Vec<u64> {
	std::mem::take(&mut BITRATES.lock().unwrap())
}

/// Fail the first `set_bitrate` asking for `bitrate`, then accept it again.
pub(crate) fn refuse_once(bitrate: u64) {
	*REFUSE.lock().unwrap() = Some(bitrate);
}

/// Fail the next `count` opens.
pub(crate) fn fail_opens(count: u32) {
	*FAIL_OPENS.lock().unwrap() = count;
}

/// Record an open, failing it while [`fail_opens`] has any left.
fn opened() -> Result<(), Error> {
	record("open");
	let mut failing = FAIL_OPENS.lock().unwrap();
	if *failing == 0 {
		return Ok(());
	}
	*failing -= 1;
	Err(Error::Unsupported("probe open refused".into()))
}

/// Fail the next flush, losing the frame held back.
pub(crate) fn lose_tail() {
	*LOSE_TAIL.lock().unwrap() = true;
}

pub(crate) struct Probe {
	/// The frame held back by one.
	held: Option<Encoded>,
	ready: VecDeque<Encoded>,
	cuts: bool,
}

impl Probe {
	pub(crate) fn open(_config: &Config) -> Result<Box<dyn Backend>, Error> {
		opened()?;
		Ok(Box::new(Self {
			held: None,
			ready: VecDeque::new(),
			cuts: true,
		}))
	}

	pub(crate) fn open_no_cut(_config: &Config) -> Result<Box<dyn Backend>, Error> {
		opened()?;
		Ok(Box::new(Self {
			held: None,
			ready: VecDeque::new(),
			cuts: false,
		}))
	}
}

impl Backend for Probe {
	fn encode(&mut self, frame: &Frame, cut: bool) -> Result<(), Error> {
		// Uncontended unless a test is holding the codec here on purpose.
		drop(GATE.lock().unwrap_or_else(|err| err.into_inner()));
		record("encode");
		// Recorded as its own event so a test can see the cut reach the codec on
		// the frame it was meant for.
		if cut {
			record("cut");
		}
		// The payload is the frame's timestamp, so a test can tell which frame a
		// packet came from independently of what it's stamped with. It rides in an
		// H.264 filler NAL, which a publisher parses and never emits as a frame, so a
		// test can publish what the probe encodes without a real codec.
		let mut payload = vec![0, 0, 0, 1, 0x0c];
		payload.extend_from_slice(frame.timestamp.as_micros()?.to_string().as_bytes());
		let payload = bytes::Bytes::from(payload);
		let previous = self.held.replace(Encoded::new(payload, frame.timestamp, cut));
		self.ready.extend(previous);
		Ok(())
	}

	fn receive(&mut self) -> Result<Option<Encoded>, Error> {
		Ok(self.ready.pop_front())
	}

	fn flush(&mut self) -> Result<(), Error> {
		record("flush");
		if std::mem::take(&mut *LOSE_TAIL.lock().unwrap()) {
			return Err(Error::codec(anyhow::anyhow!("the probe lost its tail")));
		}
		self.ready.extend(self.held.take());
		Ok(())
	}

	fn set_bitrate(&mut self, bitrate: u64) -> Result<(), Error> {
		record("set_bitrate");
		let mut refuse = REFUSE.lock().unwrap();
		if *refuse == Some(bitrate) {
			*refuse = None;
			return Err(Error::codec(anyhow::anyhow!("probe refused {bitrate} bps")));
		}
		BITRATES.lock().unwrap().push(bitrate);
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

#[cfg(feature = "openh264")]
pub(crate) struct Pipelined {
	inner: Box<dyn Backend>,
	/// The access unit held back by one.
	held: Option<Encoded>,
	ready: VecDeque<Encoded>,
}

#[cfg(feature = "openh264")]
impl Pipelined {
	pub(crate) fn open(config: &Config) -> Result<Box<dyn Backend>, Error> {
		Ok(Box::new(Self {
			inner: super::openh264::Openh264::open(config)?,
			held: None,
			ready: VecDeque::new(),
		}))
	}

	/// Hold back the newest access unit the codec readied, releasing the one before it.
	fn hold(&mut self) -> Result<(), Error> {
		while let Some(encoded) = self.inner.receive()? {
			self.ready.extend(self.held.replace(encoded));
		}
		Ok(())
	}
}

#[cfg(feature = "openh264")]
impl Backend for Pipelined {
	fn encode(&mut self, frame: &Frame, cut: bool) -> Result<(), Error> {
		self.inner.encode(frame, cut)?;
		self.hold()
	}

	fn receive(&mut self) -> Result<Option<Encoded>, Error> {
		Ok(self.ready.pop_front())
	}

	fn flush(&mut self) -> Result<(), Error> {
		self.inner.flush()?;
		self.hold()?;
		self.ready.extend(self.held.take());
		Ok(())
	}

	fn set_bitrate(&mut self, bitrate: u64) -> Result<(), Error> {
		self.inner.set_bitrate(bitrate)
	}

	fn can_cut(&self) -> bool {
		self.inner.can_cut()
	}

	fn name(&self) -> &'static str {
		PIPELINED
	}

	fn applied(&self) -> super::super::Applied {
		self.inner.applied()
	}
}

impl Drop for Probe {
	fn drop(&mut self) {
		record("drop");
	}
}
