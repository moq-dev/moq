//! What the reconnect loop retries, and what it refuses to.
//!
//! Both cases dial over plain TCP (`tcp://`), which fails fast and locally: no TLS material, no
//! QUIC handshake, no server. That keeps the assertions about the *policy* rather than about how
//! long a particular backend takes to give up.

#![cfg(feature = "tcp")]

use std::time::Duration;

/// A client whose reconnect loop escalates fast enough to assert on inside a test.
fn client(backoff: moq_native::Backoff) -> moq_native::Client {
	let mut config = moq_native::ClientConfig::default();
	config.backoff = backoff;
	config.init().expect("failed to init client")
}

/// A failure no retry can clear must surface immediately. The initial delay is far longer than the
/// timeout below, so a single retry would blow the deadline: reaching the assertion at all is the
/// proof that exactly one attempt was made.
#[tokio::test]
async fn a_deterministic_failure_makes_one_attempt() {
	let mut backoff = moq_native::Backoff::default();
	backoff.initial = Duration::from_secs(30);

	// `tcp://` has no default port, so this URL can never be dialed, however many times we try.
	let url = "tcp://localhost".parse().expect("failed to parse url");
	let reconnect = client(backoff).reconnect(url);

	let err = tokio::time::timeout(Duration::from_secs(5), reconnect.closed())
		.await
		.expect("reconnect loop retried a deterministic failure")
		.expect_err("reconnect loop stopped without an error");

	assert!(!err.is_retryable(), "gave up on a retryable error: {err}");
	assert!(
		matches!(err, moq_native::Error::Tcp(_)),
		"reported {err} instead of the failure that stopped it"
	);
}

/// A transient failure is retried, escalating, until the budget runs out. The give-up error names
/// the underlying cause so an operator sees why rather than just "timed out".
#[tokio::test]
async fn a_transient_failure_retries_until_the_budget_runs_out() {
	let mut backoff = moq_native::Backoff::default();
	backoff.initial = Duration::from_millis(20);
	backoff.max = Duration::from_millis(40);
	backoff.timeout = Duration::from_millis(200);

	// Nothing listens on port 1, so every attempt is refused: transient as far as this layer knows.
	let url = "tcp://127.0.0.1:1".parse().expect("failed to parse url");
	let started = tokio::time::Instant::now();
	let reconnect = client(backoff).reconnect(url);

	let err = tokio::time::timeout(Duration::from_secs(10), reconnect.closed())
		.await
		.expect("reconnect loop never gave up")
		.expect_err("reconnect loop stopped without an error");

	assert!(
		matches!(err, moq_native::Error::Reconnect(_)),
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
