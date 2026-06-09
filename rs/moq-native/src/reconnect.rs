use std::task::{Poll, ready};
use std::time::Duration;

use moq_net::kio;
use url::Url;

use crate::Client;

/// Exponential backoff configuration for reconnection attempts.
#[derive(Clone, Debug, clap::Args, serde::Serialize, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Backoff {
	/// Initial delay before first reconnect attempt.
	#[arg(
		id = "backoff-initial",
		long,
		default_value = "1s",
		env = "MOQ_BACKOFF_INITIAL",
		value_parser = humantime::parse_duration,
	)]
	#[serde(with = "humantime_serde")]
	pub initial: Duration,

	/// Multiplier applied to delay after each failure.
	#[arg(id = "backoff-multiplier", long, default_value_t = 2, env = "MOQ_BACKOFF_MULTIPLIER")]
	pub multiplier: u32,

	/// Maximum delay between reconnect attempts.
	#[arg(
		id = "backoff-max",
		long,
		default_value = "30s",
		env = "MOQ_BACKOFF_MAX",
		value_parser = humantime::parse_duration,
	)]
	#[serde(with = "humantime_serde")]
	pub max: Duration,

	/// Maximum time to spend retrying before giving up.
	/// Resets after each successful connection. Set to 0 for unlimited retries.
	#[arg(
		id = "backoff-timeout",
		long,
		default_value = "5m",
		env = "MOQ_BACKOFF_TIMEOUT",
		value_parser = humantime::parse_duration,
	)]
	#[serde(with = "humantime_serde")]
	pub timeout: Duration,

	/// Minimum session uptime to count as healthy. A session that closes sooner is treated as
	/// churn: the backoff keeps escalating instead of resetting, so a peer that accepts then
	/// immediately drops us can't become a tight reconnect loop. Set to 0 to disable.
	#[arg(
		id = "backoff-stable",
		long,
		default_value = "10s",
		env = "MOQ_BACKOFF_STABLE",
		value_parser = humantime::parse_duration,
	)]
	#[serde(with = "humantime_serde")]
	pub stable: Duration,
}

impl Default for Backoff {
	fn default() -> Self {
		Self {
			initial: Duration::from_secs(1),
			multiplier: 2,
			max: Duration::from_secs(30),
			timeout: Duration::from_secs(300),
			stable: Duration::from_secs(10),
		}
	}
}

/// A connection lifecycle transition reported by [`Connection::status`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
	/// A session connected (the first connect, or a reconnect after a drop).
	Connected,
	/// An established session dropped; a reconnect attempt follows.
	Disconnected,
}

/// Shared connection state, observed by consumers through a [`kio`] channel.
///
/// The channel closing (all producers dropped) is the terminal signal; `error`
/// distinguishes a permanent give-up from a graceful close.
#[derive(Default)]
struct State {
	/// Current connection status, or `None` before the first connect.
	status: Option<Status>,
	/// The currently-established session, if any. Kept so [`Connection::close`] can close it with a
	/// specific error code. Not exposed directly: handing it out would let callers outlive the loop.
	session: Option<moq_net::Session>,
	/// Set when the reconnect loop permanently gives up (reconnect timeout exceeded).
	error: Option<anyhow::Error>,
}

/// Handle to a background connection that reconnects with exponential backoff.
///
/// [`Client::connect`](crate::Client::connect) spawns a tokio task that connects, waits for the
/// session to close, then reconnects. Wire publish/consume origins on the client first; they are
/// re-attached on every reconnect. [`status`](Self::status) reports connection changes and
/// [`closed`](Self::closed) waits for the loop to stop. Dropping the handle (or calling
/// [`close`](Self::close)) stops the loop.
///
/// For a single attempt that hands you the session directly, use
/// [`Client::connect_once`](crate::Client::connect_once) instead.
pub struct Connection {
	abort: tokio::task::AbortHandle,
	state: kio::Consumer<State>,
	/// The last status returned by [`status`](Self::status), for change detection.
	last_reported: Option<Status>,
}

impl Connection {
	pub(crate) fn new(client: Client, url: Url, backoff: Backoff) -> Self {
		let producer = kio::Producer::<State>::default();
		let state = producer.consume();
		let task = tokio::spawn(async move {
			if let Err(err) = Self::run(&producer, client, url, backoff).await {
				tracing::error!(err = %format!("{err:#}"), "connection loop exited");
				if let Ok(mut state) = producer.write() {
					state.error = Some(err);
				}
			}
			// Dropping the producer here closes the channel, signaling consumers.
		});
		Self {
			abort: task.abort_handle(),
			state,
			last_reported: None,
		}
	}

	async fn run(state: &kio::Producer<State>, client: Client, url: Url, backoff: Backoff) -> anyhow::Result<()> {
		let mut delay = backoff.initial;
		let mut retry_start = tokio::time::Instant::now();
		let mut last_error: Option<anyhow::Error> = None;

		loop {
			if !backoff.timeout.is_zero() && retry_start.elapsed() > backoff.timeout {
				let timeout = backoff.timeout;
				return Err(last_error
					.map(|e| e.context(format!("reconnect timed out after {timeout:?}")))
					.unwrap_or_else(|| anyhow::anyhow!("reconnect timed out after {timeout:?}")));
			}

			tracing::info!(%url, "connecting");

			match client.connect_inner(url.clone()).await {
				Ok(session) => {
					tracing::info!(%url, version = %session.version(), "connected");
					last_error = None;
					if let Ok(mut state) = state.write() {
						state.session = Some(session.clone());
						state.status = Some(Status::Connected);
					}

					let started = tokio::time::Instant::now();
					let _ = session.closed().await;

					if let Ok(mut state) = state.write() {
						state.session = None;
						state.status = Some(Status::Disconnected);
					}

					if started.elapsed() >= backoff.stable {
						// Healthy session: reset backoff and reconnect promptly.
						tracing::warn!(%url, "session closed, reconnecting");
						delay = backoff.initial;
						retry_start = tokio::time::Instant::now();
					} else {
						// Churn: a session this short means the peer is flapping; keep backing off.
						tracing::warn!(%url, ?delay, "session closed quickly, backing off");
						tokio::time::sleep(delay).await;
						delay = std::cmp::min(delay * backoff.multiplier, backoff.max);
					}
				}
				Err(err) => {
					tracing::warn!(%url, %err, ?delay, "connection failed, retrying");
					last_error = Some(err);
					tokio::time::sleep(delay).await;
					delay = std::cmp::min(delay * backoff.multiplier, backoff.max);
				}
			}
		}
	}

	/// Stop the connection: close the current session (if any) with `error`, then end the loop.
	///
	/// After this, [`closed`](Self::closed) resolves and no further reconnects happen. Idempotent;
	/// dropping the handle does the same, minus the graceful close.
	pub fn close(&self, error: moq_net::Error) {
		if let Some(mut session) = self.state.read().session.clone() {
			session.close(error);
		}
		self.abort.abort();
	}

	/// Poll for the next connection status change since this handle last reported one.
	///
	/// `Ready(Ok(status))` on a change, `Ready(Err)` once the loop has stopped (the give-up error,
	/// or a generic one when the handle is dropped), `Pending` otherwise.
	pub fn poll_status(&mut self, waiter: &kio::Waiter) -> Poll<anyhow::Result<Status>> {
		let last = self.last_reported;
		let status = match ready!(self.state.poll(waiter, |state| match state.status {
			Some(status) if Some(status) != last => Poll::Ready(status),
			_ => Poll::Pending,
		})) {
			Ok(status) => status,
			Err(state) => return Poll::Ready(Err(terminal(&state))),
		};

		self.last_reported = Some(status);
		Poll::Ready(Ok(status))
	}

	/// Wait until the connection status changes from what this handle last reported.
	///
	/// Returns the current [`Status`]. The loop alternates `Connected`/`Disconnected`, so successive
	/// calls alternate too; but a status that flips and flips back before the caller polls is
	/// reported once. This tracks the *current* state, not every edge.
	pub async fn status(&mut self) -> anyhow::Result<Status> {
		kio::wait(|waiter| self.poll_status(waiter)).await
	}

	/// Poll whether the connection loop has stopped.
	///
	/// `Ready(Err)` if it permanently gave up (reconnect timeout exceeded), `Ready(Ok(()))` if
	/// stopped by dropping the handle or calling [`close`](Self::close), `Pending` while running.
	pub fn poll_closed(&self, waiter: &kio::Waiter) -> Poll<anyhow::Result<()>> {
		ready!(self.state.poll_closed(waiter));
		Poll::Ready(match &self.state.read().error {
			Some(err) => Err(anyhow::anyhow!("{err:#}")),
			None => Ok(()),
		})
	}

	/// Wait until the connection loop stops.
	pub async fn closed(&self) -> anyhow::Result<()> {
		kio::wait(|waiter| self.poll_closed(waiter)).await
	}
}

impl Drop for Connection {
	fn drop(&mut self) {
		self.abort.abort();
	}
}

/// The terminal error read from a closed channel's final state.
fn terminal(state: &State) -> anyhow::Error {
	match &state.error {
		Some(err) => anyhow::anyhow!("{err:#}"),
		None => anyhow::anyhow!("reconnect stopped"),
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn test_backoff_default() {
		let backoff = Backoff::default();
		assert_eq!(backoff.initial, Duration::from_secs(1));
		assert_eq!(backoff.multiplier, 2);
		assert_eq!(backoff.max, Duration::from_secs(30));
		assert_eq!(backoff.timeout, Duration::from_secs(300));
		assert_eq!(backoff.stable, Duration::from_secs(10));
	}
}
