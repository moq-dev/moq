//! A slice of moq-ffi bound by either UniFFI or BoltFFI, so the generators compare on
//! the same Rust (see `README.md`).
//!
//! The `uniffi` or `boltffi` feature picks one generator per build. Exports carry
//! BoltFFI's markers literally, because BoltFFI scans source text and cannot see a marker
//! inside `cfg_attr`; the UniFFI build imports same-named shims that expand to UniFFI's
//! attributes. Only UniFFI's extra attributes (`Object`, `constructor`) use `cfg_attr`.
//! The shapes follow moq-ffi's, so a call costs what the same moq-ffi call does, minus
//! moq-ffi's extra bookkeeping.

#[cfg(all(feature = "uniffi", feature = "boltffi"))]
compile_error!("enable one generator: `uniffi` or `boltffi`");

#[cfg(feature = "uniffi")]
uniffi::setup_scaffolding!();

#[cfg(feature = "boltffi")]
use boltffi::{data, error, export};
#[cfg(feature = "uniffi")]
use moq_ffi_gen_uniffi_shim::{data, error, export};

use std::sync::Mutex;

#[cfg(feature = "client")]
mod client;
#[cfg(feature = "client")]
pub use client::*;

// No thiserror: its derive claims BoltFFI's bare `#[error]` marker as its own attribute.
// `reason`, not `message`: UniFFI's Kotlin exception would clash with `Throwable.message`.
/// An error crossing the boundary.
#[derive(Debug)]
#[error]
pub enum MoqError {
	/// The handle was already closed.
	Closed,
	/// The track ended: no more groups.
	Ended,
	/// moq-net refused the operation.
	Net { reason: String },
}

impl std::fmt::Display for MoqError {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		match self {
			Self::Closed => write!(f, "closed"),
			Self::Ended => write!(f, "ended"),
			Self::Net { reason } => write!(f, "{reason}"),
		}
	}
}

impl std::error::Error for MoqError {}

impl From<moq_net::Error> for MoqError {
	fn from(err: moq_net::Error) -> Self {
		Self::Net {
			reason: err.to_string(),
		}
	}
}

/// A frame's payload and presentation timestamp.
#[derive(Clone)]
#[data]
pub struct MoqFrame {
	pub payload: Vec<u8>,
	pub timestamp_us: Option<u64>,
}

/// A track's delivery metadata, the record the call benchmark returns.
#[derive(Clone)]
#[data]
pub struct MoqTrackInfo {
	pub name: String,
	pub priority: u8,
	pub timescale: Option<u64>,
}

/// Does nothing: the floor of a sync call.
#[export]
pub fn noop() {}

/// Resolves at once: the floor of an async call.
#[export]
pub async fn noop_async() {}

/// Returns pending once and wakes itself, so the host pays one continuation and re-poll.
#[export]
pub async fn yield_once() {
	let mut yielded = false;
	std::future::poll_fn(|cx| {
		if yielded {
			return std::task::Poll::Ready(());
		}
		yielded = true;
		cx.waker().wake_by_ref();
		std::task::Poll::Pending
	})
	.await
}

/// Publishes broadcasts under a cache budget, so a long benchmark keeps memory flat.
#[cfg_attr(feature = "uniffi", derive(uniffi::Object))]
pub struct MoqOriginProducer {
	inner: moq_net::origin::Producer,
}

#[export]
impl MoqOriginProducer {
	/// An origin whose cached groups total at most `cache_capacity_bytes`.
	#[cfg_attr(feature = "uniffi", uniffi::constructor)]
	pub fn new(cache_capacity_bytes: u64) -> Self {
		let mut config = moq_net::origin::Config::default();
		let cache = moq_net::cache::Config::default()
			.with_capacity(cache_capacity_bytes)
			.with_expiry(config.pool.expiry());
		config.pool = moq_net::cache::Pool::new(cache);
		let (inner, driver) = moq_net::origin::Producer::new(config);
		spawn(async move {
			moq_net::time::run(driver).await;
		});
		Self { inner }
	}

	/// Create a broadcast at `path`.
	pub fn create_broadcast(&self, path: String) -> Result<MoqBroadcastProducer, MoqError> {
		Ok(MoqBroadcastProducer::from_inner(
			self.inner.create_broadcast(path.as_str())?,
		))
	}
}

#[cfg_attr(feature = "uniffi", derive(uniffi::Object))]
pub struct MoqBroadcastProducer {
	inner: moq_net::broadcast::Producer,
}

impl MoqBroadcastProducer {
	fn from_inner(inner: moq_net::broadcast::Producer) -> Self {
		Self { inner }
	}
}

impl Default for MoqBroadcastProducer {
	fn default() -> Self {
		Self::new()
	}
}

#[export]
impl MoqBroadcastProducer {
	/// A standalone broadcast, outside any origin.
	#[cfg_attr(feature = "uniffi", uniffi::constructor)]
	pub fn new() -> Self {
		Self::from_inner(moq_net::broadcast::Info::new().produce())
	}

	/// Publish a track timed in microseconds.
	pub fn publish_track(&self, name: String) -> Result<MoqTrackProducer, MoqError> {
		let info = moq_net::track::Info::default().with_timescale(moq_net::Timescale::MICRO);
		let track = self.inner.create_track(name, Some(info))?;
		Ok(MoqTrackProducer {
			inner: Mutex::new(Some(track)),
		})
	}
}

#[cfg_attr(feature = "uniffi", derive(uniffi::Object))]
pub struct MoqTrackProducer {
	inner: Mutex<Option<moq_net::track::Producer>>,
}

#[export]
impl MoqTrackProducer {
	/// Subscribe to this track locally.
	pub fn consume(&self) -> Result<MoqTrackConsumer, MoqError> {
		let guard = self.inner.lock().unwrap();
		let track = guard.as_ref().ok_or(MoqError::Closed)?;
		Ok(MoqTrackConsumer {
			inner: Mutex::new(track.subscribe(None)),
		})
	}

	/// Start the next group.
	pub fn append_group(&self) -> Result<MoqGroupProducer, MoqError> {
		let mut guard = self.inner.lock().unwrap();
		let track = guard.as_mut().ok_or(MoqError::Closed)?;
		let group = track.append_group()?;
		Ok(MoqGroupProducer {
			sequence: group.sequence,
			inner: Mutex::new(Some(group)),
		})
	}

	/// End the track.
	pub fn finish(&self) -> Result<(), MoqError> {
		let mut guard = self.inner.lock().unwrap();
		guard.as_mut().ok_or(MoqError::Closed)?.finish()?;
		Ok(())
	}
}

#[cfg_attr(feature = "uniffi", derive(uniffi::Object))]
pub struct MoqGroupProducer {
	sequence: u64,
	inner: Mutex<Option<moq_net::group::Producer>>,
}

#[export]
impl MoqGroupProducer {
	/// This group's sequence number: the plain sync getter the call benchmark times.
	pub fn sequence(&self) -> u64 {
		self.sequence
	}

	/// Read this group back locally.
	pub fn consume(&self) -> Result<MoqGroupConsumer, MoqError> {
		let guard = self.inner.lock().unwrap();
		let group = guard.as_ref().ok_or(MoqError::Closed)?;
		Ok(MoqGroupConsumer::new(group.consume()))
	}

	/// Append `frame`, copying its payload into Rust as moq-ffi does.
	pub fn write_frame(&self, frame: MoqFrame) -> Result<(), MoqError> {
		let timestamp = frame
			.timestamp_us
			.map(moq_net::Timestamp::from_micros)
			.transpose()
			.map_err(|err| MoqError::Net {
				reason: err.to_string(),
			})?;
		let mut guard = self.inner.lock().unwrap();
		let group = guard.as_mut().ok_or(MoqError::Closed)?;
		group.write_frame(timestamp, frame.payload)?;
		Ok(())
	}

	/// Close the group; readers see its end.
	pub fn finish(&self) -> Result<(), MoqError> {
		let mut guard = self.inner.lock().unwrap();
		guard.as_mut().ok_or(MoqError::Closed)?.finish()?;
		Ok(())
	}
}

#[cfg_attr(feature = "uniffi", derive(uniffi::Object))]
pub struct MoqTrackConsumer {
	inner: Mutex<moq_net::track::Subscriber>,
}

#[export]
impl MoqTrackConsumer {
	/// The track's metadata: the record the call benchmark returns.
	pub fn info(&self) -> MoqTrackInfo {
		let inner = self.inner.lock().unwrap();
		let info = inner.info();
		MoqTrackInfo {
			name: inner.name().to_string(),
			priority: info.priority,
			timescale: info.timescale.map(|t| t.as_u64()),
		}
	}

	/// The next group in arrival order; [`MoqError::Ended`] once the track ends.
	///
	/// moq-ffi returns `None` at the end instead, but neither generator takes that
	/// shape as-is: UniFFI lowers an optional object only as `Option<Arc<T>>`, which
	/// BoltFFI cannot represent, and BoltFFI's Python backend skips any class with a
	/// method returning `Option<T>` for a class `T`.
	pub async fn recv_group(&self) -> Result<MoqGroupConsumer, MoqError> {
		let group = kio::wait(|waiter| self.inner.lock().unwrap().poll_recv_group(waiter)).await?;
		group.map(MoqGroupConsumer::new).ok_or(MoqError::Ended)
	}
}

#[cfg_attr(feature = "uniffi", derive(uniffi::Object))]
pub struct MoqGroupConsumer {
	sequence: u64,
	inner: Mutex<moq_net::group::Consumer>,
}

impl MoqGroupConsumer {
	fn new(group: moq_net::group::Consumer) -> Self {
		Self {
			sequence: group.sequence,
			inner: Mutex::new(group),
		}
	}
}

#[export]
impl MoqGroupConsumer {
	/// This group's sequence number.
	pub fn sequence(&self) -> u64 {
		self.sequence
	}

	/// The next frame, copied out of Rust as moq-ffi does, or `None` once the group ends.
	pub async fn read_frame(&self) -> Result<Option<MoqFrame>, MoqError> {
		let frame = kio::wait(|waiter| self.inner.lock().unwrap().poll_read_frame(waiter)).await?;
		Ok(frame.map(|frame| MoqFrame {
			payload: frame.payload.to_vec(),
			timestamp_us: frame.timestamp.map(|t| t.as_micros() as u64),
		}))
	}
}

/// The worker runtime that drives timers and I/O beside host-polled calls.
#[cfg(not(target_arch = "wasm32"))]
fn runtime() -> &'static tokio::runtime::Runtime {
	static RUNTIME: std::sync::OnceLock<tokio::runtime::Runtime> = std::sync::OnceLock::new();
	RUNTIME.get_or_init(|| {
		tokio::runtime::Builder::new_multi_thread()
			.thread_name("moq-ffi-gen")
			.enable_all()
			.build()
			.unwrap()
	})
}

#[cfg(not(target_arch = "wasm32"))]
fn spawn(fut: impl std::future::Future<Output = ()> + Send + 'static) {
	runtime().spawn(fut);
}

// BoltFFI requires every class to be `Send + Sync`, but moq-net's handles are not on
// wasm32. Its `#[export(single_threaded)]` opt-out cannot depend on the target, since
// BoltFFI ignores markers inside `cfg_attr`, so assert it here instead: wasm32 without
// threads never moves a handle across threads. UniFFI's `wasm-unstable-single-threaded`
// feature drops the same bounds.
#[cfg(all(feature = "boltffi", target_arch = "wasm32"))]
mod single_threaded {
	use super::*;

	macro_rules! assume_single_threaded {
		($($ty:ty),*) => {$(
			unsafe impl Send for $ty {}
			unsafe impl Sync for $ty {}
		)*};
	}

	assume_single_threaded!(
		MoqOriginProducer,
		MoqBroadcastProducer,
		MoqTrackProducer,
		MoqGroupProducer,
		MoqTrackConsumer,
		MoqGroupConsumer
	);
}

#[cfg(target_arch = "wasm32")]
fn spawn(fut: impl std::future::Future<Output = ()> + 'static) {
	web_async::spawn(fut);
}
