//! wasm32 model-layer tests.
//!
//! moq-net's model layer (Origin/Broadcast/Track/Group/Frame) is transport-
//! independent, so it can be exercised in-process on wasm without a
//! WebTransport session. This covers both directions (produce + consume) plus
//! the wasm timestamp clock that the producer path depends on.
//!
//! Run (bypassing `wasm-pack test`, which builds the crate's native-only lib
//! unit tests too. They use `tokio::spawn` and don't compile on wasm):
//!
//! ```sh
//! CARGO_TARGET_WASM32_UNKNOWN_UNKNOWN_RUNNER=wasm-bindgen-test-runner \
//! RUSTFLAGS='--cfg=web_sys_unstable_apis --cfg=getrandom_backend="wasm_js"' \
//! cargo test --test wasm -p moq-net --target wasm32-unknown-unknown
//! ```
//!
//! Runs under Node (default). `performance.now()` / `Date.now()` back the
//! clock there just as in a browser; these model-layer tests need no
//! WebTransport. Add `wasm_bindgen_test_configure!(run_in_browser)` to run under
//! headless Chrome (the subscriber's real environment) once chromedriver is set.
#![cfg(target_arch = "wasm32")]

use bytes::Bytes;
use moq_net::{Broadcast, Time, Track};
use wasm_bindgen_test::*;

/// The producer timestamp clock works on wasm: `Time::now()` (which flows through
/// the web_async clock) returns a sane, non-decreasing wall-clock time.
/// On the old code this panicked (`std::time` has no clock on wasm32).
#[wasm_bindgen_test]
fn timescale_now_is_sane_and_monotonic() {
	let a = Time::now();
	let b = Time::now();

	// A real post-2020 wall-clock time in ms (2020-01-01 = 1_577_836_800_000).
	// Proves the web_async wall clock actually resolved.
	assert!(
		a.as_millis() > 1_577_836_800_000,
		"timestamp before 2020. wall clock not working: {}",
		a.as_millis()
	);
	// Monotonic non-decreasing.
	assert!(b >= a, "time went backwards: {} < {}", b.as_millis(), a.as_millis());
}

/// Bidirectional model round-trip in-process on wasm: produce a track + frame,
/// then consume it back. Exercises the produce path (which stamps groups via the
/// wasm `web_async::time` clock) and the consume path together.
#[wasm_bindgen_test]
async fn produce_consume_frame_roundtrip() {
	let mut broadcast = Broadcast::new().produce();
	let mut track = broadcast.create_track(Track::new("stream")).unwrap();
	let consumer = broadcast.consume();
	let mut sub = consumer.subscribe_track(&Track::new("stream")).unwrap();

	// Producer side: write a frame (creates a group, timestamped via the wasm clock).
	track.write_frame(Bytes::from_static(b"hello-wasm")).unwrap();

	// Consumer side: read it back.
	let frame = sub.read_frame().await.unwrap();
	assert_eq!(frame.as_deref(), Some(&b"hello-wasm"[..]), "frame did not round-trip");
}
