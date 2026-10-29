#![warn(missing_docs)]
#![cfg_attr(
	test,
	expect(clippy::disallowed_methods, reason = "tests read the host clock directly")
)]

//! UniFFI bindings for [`moq_net`].
//!
//! Provides a Kotlin/Swift-compatible API for real-time pub/sub over QUIC.
//! Uses async UniFFI objects instead of callbacks for a native async experience.

// uniffi lowers every exported object as an `Arc<T>`, and on wasm moq-net's handles are
// `Rc`-backed, so clippy sees a pointlessly atomic `Arc` at every construction site. The
// `Arc` is uniffi's ABI rather than our choice, and the target is single-threaded, so the
// wasted atomics are what one type serving both targets costs.
#![cfg_attr(
	target_arch = "wasm32",
	expect(
		clippy::arc_with_non_send_sync,
		reason = "`Arc` is uniffi's ABI, and wasm is single-threaded"
	)
)]

// Counts moq's heap for the host leak tests; see `moq_ffi_test_allocated`.
#[cfg(all(feature = "testing", not(target_arch = "wasm32")))]
mod alloc;
#[cfg(target_os = "android")]
mod android;
// The QUIC server and moq-native's logger have no browser equivalent, and the
// native codecs are both browser-inapplicable (WebCodecs does this) and optional,
// since they are the heaviest thing in the dependency graph.
#[cfg(all(feature = "audio", not(target_arch = "wasm32")))]
pub mod audio;
pub mod auth;
pub mod bandwidth;
pub mod consumer;
pub mod demand;
pub mod error;
mod ffi;
pub mod json;
#[cfg(not(target_arch = "wasm32"))]
mod log;
pub mod media;
pub mod origin;
pub mod producer;
#[cfg(not(target_arch = "wasm32"))]
pub mod server;
pub mod session;
#[cfg(target_arch = "wasm32")]
mod transport;
#[cfg(all(feature = "video", not(target_arch = "wasm32")))]
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
#[cfg(not(target_arch = "wasm32"))]
#[unsafe(no_mangle)]
pub extern "C" fn moq_ffi_shutdown() {
	ffi::shutdown();
}

/// Start the runtime on a clock that moves only by [`moq_ffi_test_clock_advance`].
///
/// Test builds only (the `testing` feature), for host test suites that keep native time in
/// step with their own loop. Call it before anything else uses the bindings: false means the
/// runtime already started on the host clock.
#[cfg(all(feature = "testing", not(target_arch = "wasm32")))]
#[unsafe(no_mangle)]
pub extern "C" fn moq_ffi_test_clock_start() -> bool {
	ffi::start_test_clock()
}

/// Let `nanos` pass on the test clock, waking every timer due by then.
///
/// False without a test clock, or when the advance would run past the clock's range, which
/// would otherwise panic across this boundary and abort the host.
#[cfg(all(feature = "testing", not(target_arch = "wasm32")))]
#[unsafe(no_mangle)]
pub extern "C" fn moq_ffi_test_clock_advance(nanos: u64) -> bool {
	ffi::test_clock().is_some_and(|clock| clock.advance(std::time::Duration::from_nanos(nanos)).is_some())
}

/// The test clock's nanoseconds since it started, or `u64::MAX` without one.
#[cfg(all(feature = "testing", not(target_arch = "wasm32")))]
#[unsafe(no_mangle)]
pub extern "C" fn moq_ffi_test_clock_now() -> u64 {
	ffi::test_clock().map_or(u64::MAX, |clock| {
		clock.now().duration_since(moq_clock::Instant::ZERO).as_nanos() as u64
	})
}

// The test suite drives a native relay over a tokio runtime.
#[cfg(all(test, not(target_arch = "wasm32")))]
mod test;

// The binding benchmark's Rust baselines; ignored unless a run asks for them (see bench/README.md).
#[cfg(all(test, unix))]
mod bench;
