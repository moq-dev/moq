use std::task::{Poll, ready};
use std::time::Duration;

use moq_net::kio;
use url::Url;

use crate::{Client, Error};

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
}

impl Default for Backoff {
	fn default() -> Self {
		Self {
			initial: Duration::from_secs(1),
			multiplier: 2,
			max: Duration::from_secs(30),
			timeout: Duration::from_secs(300),
		}
	}
}

/// A connection lifecycle transition reported by [`Reconnect::status`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
	/// A session connected (the first connect, or a reconnect after a drop).
	Connected,
	/// An established session dropped; a reconnect attempt follows.
	Disconnected,
}

/// Shared reconnect state, observed by consumers through a [`kio`] channel.
///
/// The channel closing (all producers dropped) is the terminal signal; `error`
/// distinguishes a permanent give-up from a graceful close.
#[derive(Default)]
struct State {
	/// Current connection status, or `None` before the first connect.
	status: Option<Status>,
	/// Set when the reconnect loop permanently gives up (reconnect timeout exceeded).
	error: Option<Error>,
	/// The currently-connected session, or `None` while reconnecting. Read by
	/// [`ConnectionStatsReader`] to snapshot live connection stats.
	session: Option<moq_net::Session>,
}

/// A cloneable read handle for the live connection stats of a [`Reconnect`] loop.
///
/// Obtained via [`Reconnect::stats`]. [`stats`](Self::stats) returns `None` while the loop is
/// between connections (reconnecting), and `Some` snapshot while a session is established.
#[derive(Clone)]
pub struct ConnectionStatsReader {
	state: kio::Consumer<State>,
}

impl ConnectionStatsReader {
	/// Snapshot the current connection's stats, or `None` if not currently connected.
	pub fn stats(&self) -> Option<moq_net::ConnectionStats> {
		self.state.read().session.as_ref().map(moq_net::Session::stats)
	}
}

/// Handle to a background reconnect loop.
///
/// Spawns a tokio task that connects, waits for session close, then reconnects with exponential
/// backoff. [`status`](Self::status) reports connection changes; [`closed`](Self::closed) waits for
/// the loop to stop. Dropping the handle aborts the background task.
pub struct Reconnect {
	abort: tokio::task::AbortHandle,
	state: kio::Consumer<State>,
	/// The last status returned by [`status`](Self::status), for change detection.
	last_reported: Option<Status>,
}

impl Reconnect {
	pub(crate) fn new(client: Client, url: Url, backoff: Backoff) -> Self {
		let producer = kio::Producer::<State>::default();
		let state = producer.consume();
		let task = tokio::spawn(async move {
			if let Err(err) = Self::run(&producer, client, url, backoff).await {
				tracing::error!(%err, "reconnect loop exited");
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

	async fn run(state: &kio::Producer<State>, client: Client, url: Url, backoff: Backoff) -> crate::Result<()> {
		let mut delay = backoff.initial;
		let mut retry_start = tokio::time::Instant::now();
		let mut last_error: Option<Error> = None;

		loop {
			if !backoff.timeout.is_zero() && retry_start.elapsed() > backoff.timeout {
				let timeout = backoff.timeout;
				let msg = match last_error {
					Some(err) => format!("reconnect timed out after {timeout:?}: {err}"),
					None => format!("reconnect timed out after {timeout:?}"),
				};
				return Err(Error::Reconnect(msg));
			}

			tracing::info!(%url, "connecting");

			match client.connect(url.clone()).await {
				Ok(cs) => {
					tracing::info!(%url, "connected");
					delay = backoff.initial;
					last_error = None;
					if let Ok(mut state) = state.write() {
						state.status = Some(Status::Connected);
						state.session = Some(cs.clone());
					}
					let _ = cs.closed().await;
					tracing::warn!(%url, "session closed, reconnecting");
					if let Ok(mut state) = state.write() {
						state.status = Some(Status::Disconnected);
						state.session = None;
					}
					retry_start = tokio::time::Instant::now();
				}
				Err(err) => {
					if err.is_auth() {
						return Err(err);
					}
					tracing::warn!(%url, %err, ?delay, "connection failed, retrying");
					last_error = Some(err);
					tokio::time::sleep(delay).await;
					delay = std::cmp::min(delay * backoff.multiplier, backoff.max);
				}
			}
		}
	}

	/// Poll for the next connection status change since this handle last reported one.
	///
	/// `Ready(Ok(status))` on a change, `Ready(Err)` once the loop has stopped (the give-up error,
	/// or a generic one when the handle is dropped), `Pending` otherwise.
	pub fn poll_status(&mut self, waiter: &kio::Waiter) -> Poll<crate::Result<Status>> {
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
	pub async fn status(&mut self) -> crate::Result<Status> {
		kio::wait(|waiter| self.poll_status(waiter)).await
	}

	/// Poll whether the reconnect loop has stopped.
	///
	/// `Ready(Err)` if it permanently gave up (reconnect timeout exceeded), `Ready(Ok(()))` if
	/// stopped by dropping the handle, `Pending` while it's still running.
	pub fn poll_closed(&self, waiter: &kio::Waiter) -> Poll<crate::Result<()>> {
		ready!(self.state.poll_closed(waiter));
		Poll::Ready(match &self.state.read().error {
			Some(err) => Err(err.clone()),
			None => Ok(()),
		})
	}

	/// Wait until the reconnect loop stops.
	pub async fn closed(&self) -> crate::Result<()> {
		kio::wait(|waiter| self.poll_closed(waiter)).await
	}

	/// A cloneable handle for reading the current connection's stats.
	///
	/// The handle keeps working across reconnects, reporting `None` between connections.
	pub fn stats(&self) -> ConnectionStatsReader {
		ConnectionStatsReader {
			state: self.state.clone(),
		}
	}
}

impl Drop for Reconnect {
	fn drop(&mut self) {
		self.abort.abort();
	}
}

/// The terminal error read from a closed channel's final state.
fn terminal(state: &State) -> Error {
	match &state.error {
		Some(err) => err.clone(),
		None => Error::Reconnect("reconnect stopped".to_string()),
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
	}
}
