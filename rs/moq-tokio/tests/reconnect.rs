//! What ends the reconnect loop.
//!
//! Dials over plain TCP (`tcp://`), which fails fast and locally: no TLS material, no QUIC
//! handshake, no server. That keeps the assertion about the *budget* rather than about how long a
//! particular backend takes to give up.

#![cfg(feature = "tcp")]

use std::time::Duration;

/// A client whose reconnect loop escalates fast enough to assert on inside a test.
fn client(backoff: moq_tokio::Backoff) -> moq_tokio::Client {
	let mut config = moq_tokio::connect::Config::default();
	config.backoff = backoff;
	config.init(Default::default()).expect("failed to init client")
}

/// A transient failure is retried, escalating, until the budget runs out. The give-up error names
/// the underlying cause so an operator sees why rather than just "timed out".
#[tokio::test]
async fn a_transient_failure_retries_until_the_budget_runs_out() {
	let mut backoff = moq_tokio::Backoff::default();
	backoff.initial = Some(Duration::from_millis(20));
	backoff.max = Some(Duration::from_millis(40));
	backoff.timeout = Some(Duration::from_millis(200));

	// Nothing listens on port 1, so every attempt is refused: transient as far as this layer knows.
	let url: url::Url = "tcp://127.0.0.1:1".parse().expect("failed to parse url");
	let started = tokio::time::Instant::now();
	let reconnect = client(backoff).connect(url);

	let err = tokio::time::timeout(Duration::from_secs(10), reconnect.closed())
		.await
		.expect("reconnect loop never gave up")
		.expect_err("reconnect loop stopped without an error");

	assert!(
		matches!(err, moq_tokio::Error::Reconnect(_)),
		"stopped with {err} rather than exhausting the budget"
	);
	assert_ne!(
		err.to_string(),
		"reconnect timed out after 200ms",
		"give-up error lost the underlying cause"
	);
	// The budget is spent on sleeping between attempts, so reaching it takes at least most of it.
	// Jitter draws each delay from the top half of its window, hence half rather than the whole.
	assert!(
		started.elapsed() >= Duration::from_millis(100),
		"gave up after {:?} without retrying",
		started.elapsed()
	);
}
