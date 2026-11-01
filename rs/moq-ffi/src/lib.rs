#![warn(missing_docs)]

//! UniFFI bindings for [`moq_net`].
//!
//! Provides a Kotlin/Swift-compatible API for real-time pub/sub over QUIC.
//! Uses async UniFFI objects instead of callbacks for a native async experience.

// Counts moq's heap for the host leak tests; see `moq_ffi_test_allocated`.
#[cfg(feature = "testing")]
mod alloc;
#[cfg(target_os = "android")]
mod android;
// The native codecs are optional, since they are the heaviest thing in the dependency graph.
#[cfg(feature = "audio")]
pub mod audio;
pub mod auth;
pub mod bandwidth;
pub mod consumer;
pub mod demand;
pub mod error;
mod ffi;
pub mod json;
mod log;
pub mod media;
pub mod origin;
pub mod producer;
pub mod server;
pub mod session;
#[cfg(feature = "video")]
pub mod video;

uniffi::setup_scaffolding!("moq");

/// Stop the runtime workers before the host runtime goes away.
///
/// Every task is dropped, so a call parked on the runtime resolves `Cancelled` and so does any
/// later one; the handles stay safe to use and drop. This is for process exit, not a reset.
///
/// It exists because the runtime workers call back into the host to wake an awaiting future,
/// and CPython (before 3.14) terminates a foreign thread that touches the interpreter once
/// finalization has begun, by `pthread_exit`, whose forced unwind cannot pass through Rust
/// frames and aborts the process. The Python binding calls this from `atexit`, which runs
/// before finalization, so every worker finishes its current callback and never starts another.
/// A plain C symbol rather than a uniffi export: no other binding kills threads this way.
///
/// Call it from a host thread. A callback running on a runtime worker would wait on itself.
#[unsafe(no_mangle)]
pub extern "C" fn moq_ffi_shutdown() {
	ffi::shutdown();
}

/// Start the runtime on a clock that moves only by [`moq_ffi_test_clock_advance`].
///
/// Test builds only (the `testing` feature), for host test suites that keep native time in
/// step with their own loop. Call it before anything else uses the bindings: false means the
/// runtime already started on the host clock.
#[cfg(feature = "testing")]
#[unsafe(no_mangle)]
pub extern "C" fn moq_ffi_test_clock_start() -> bool {
	ffi::start_test_clock()
}

/// Let `nanos` pass on the test clock, waking every timer due by then.
///
/// False without a test clock, or when the advance would run past the clock's range, which
/// would otherwise panic across this boundary and abort the host.
#[cfg(feature = "testing")]
#[unsafe(no_mangle)]
pub extern "C" fn moq_ffi_test_clock_advance(nanos: u64) -> bool {
	ffi::test_clock().is_some_and(|clock| clock.advance(std::time::Duration::from_nanos(nanos)).is_some())
}

/// The test clock's nanoseconds since it started, or `u64::MAX` without one.
#[cfg(feature = "testing")]
#[unsafe(no_mangle)]
pub extern "C" fn moq_ffi_test_clock_now() -> u64 {
	ffi::test_clock().map_or(u64::MAX, |clock| {
		clock.now().duration_since(moq_clock::Instant::ZERO).as_nanos() as u64
	})
}

// The test suite drives a native relay over a tokio runtime.
#[cfg(test)]
mod test;

// The binding benchmark's Rust baselines; ignored unless a run asks for them (see bench/README.md).
#[cfg(all(test, unix))]
#[expect(
	clippy::disallowed_methods,
	clippy::disallowed_types,
	reason = "the binding benchmark measures real throughput and call cost"
)]
mod bench;
