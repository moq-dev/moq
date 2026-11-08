//! A stand-in AAC encoder, so the AAC front end is testable on a host with no
//! platform encoder. A test opts in with [`install`]; every other test sees the
//! real tiers.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::sync::{Arc, Weak};

use bytes::Bytes;

use super::Backend;
use crate::Error;
use crate::encode::{Config, Encoded};

pub(crate) const NAME: &str = "stub";

/// The AudioToolbox AAC-LC encoder delay, which is what a real backend reports.
pub(crate) const DELAY: usize = 2112;

thread_local! {
	static INSTALLED: RefCell<Weak<()>> = const { RefCell::new(Weak::new()) };
}

/// Stand the stub in for the platform tier on this thread until the guard drops.
pub(crate) fn install() -> Installed {
	let resource = Arc::new(());
	INSTALLED.set(Arc::downgrade(&resource));
	Installed(resource)
}

/// Whether this thread has the stub installed.
pub(super) fn installed() -> bool {
	INSTALLED.with(|installed| installed.borrow().strong_count() > 0)
}

/// Uninstalls the stub on drop.
pub(crate) struct Installed(Arc<()>);

impl Installed {
	/// The backend resources still owned by encoders opened under this guard.
	pub(crate) fn resources(&self) -> usize {
		Arc::strong_count(&self.0) - 1
	}
}

impl Drop for Installed {
	fn drop(&mut self) {
		INSTALLED.set(Weak::new());
	}
}

/// Emits each frame's index as its payload, and cannot retune.
///
/// Pipelines like a platform encoder: it holds every other frame and releases
/// it with the next, so submissions alternate between readying no packet and two,
/// and a tail waits for [`flush`](Backend::flush).
pub(crate) struct Stub {
	_resource: Arc<()>,
	bitrate: u64,
	frames: u64,
	held: Option<Encoded>,
	ready: VecDeque<Encoded>,
}

impl Stub {
	pub(super) fn open(config: &Config) -> Result<Box<dyn Backend>, Error> {
		Ok(Box::new(Self {
			_resource: INSTALLED.with(|installed| installed.borrow().upgrade().unwrap()),
			bitrate: config.bitrate.map_or(128_000, |rate| rate.as_bps()),
			frames: 0,
			held: None,
			ready: VecDeque::new(),
		}))
	}
}

impl Backend for Stub {
	fn encode(&mut self, _pcm: &[f32]) -> Result<(), Error> {
		let packet = Encoded::new(Bytes::copy_from_slice(&self.frames.to_be_bytes()));
		self.frames += 1;
		match self.held.take() {
			Some(held) => self.ready.extend([held, packet]),
			None => self.held = Some(packet),
		}
		Ok(())
	}

	fn receive(&mut self) -> Result<Option<Encoded>, Error> {
		Ok(self.ready.pop_front())
	}

	fn flush(&mut self) -> Result<(), Error> {
		self.ready.extend(self.held.take());
		Ok(())
	}

	fn reset(&mut self) {
		self.frames = 0;
		self.held = None;
		self.ready.clear();
	}

	fn set_bitrate(&mut self, bitrate: u64) -> Result<(), Error> {
		match bitrate == self.bitrate {
			true => Ok(()),
			false => Err(Error::Unsupported("the stub cannot change rate mid-stream".into())),
		}
	}

	fn bitrate(&self) -> u64 {
		self.bitrate
	}

	fn delay(&self) -> usize {
		DELAY
	}

	fn name(&self) -> &str {
		NAME
	}
}
