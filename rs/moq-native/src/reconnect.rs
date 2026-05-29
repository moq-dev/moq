use std::sync::Arc;
use std::task::Poll;
use std::time::Duration;

use moq_net::conducer;
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

/// Shared reconnect state, observed by consumers through a [`conducer`] channel.
///
/// The channel closing (all producers dropped) is the terminal signal; `error`
/// distinguishes a permanent give-up from a graceful close.
#[derive(Default)]
struct State {
	/// Incremented on each successful connect: 1 after the first, 2 after the first reconnect, etc.
	connects: u64,
	/// Incremented each time an established session drops.
	disconnects: u64,
	/// Set when the reconnect loop permanently gives up (reconnect timeout exceeded).
	error: Option<Arc<anyhow::Error>>,
}

/// Handle to a background reconnect loop.
///
/// Spawns a tokio task that connects, waits for session close, then reconnects
/// with exponential backoff. [`connected`](Self::connected) and
/// [`disconnected`](Self::disconnected) report each transition;
/// [`closed`](Self::closed) is the only terminal signal. Dropping the handle
/// aborts the background task.
pub struct Reconnect {
	abort: tokio::task::AbortHandle,
	state: conducer::Consumer<State>,
}

impl Reconnect {
	pub(crate) fn new(client: Client, url: Url, backoff: Backoff) -> Self {
		let producer = conducer::Producer::<State>::default();
		let state = producer.consume();
		let task = tokio::spawn(async move {
			if let Err(err) = Self::run(&producer, client, url, backoff).await {
				tracing::error!(err = %format!("{err:#}"), "reconnect loop exited");
				if let Ok(mut state) = producer.write() {
					state.error = Some(Arc::new(err));
				}
			}
			// Dropping the producer here closes the channel, signaling consumers.
		});
		Self {
			abort: task.abort_handle(),
			state,
		}
	}

	async fn run(state: &conducer::Producer<State>, client: Client, url: Url, backoff: Backoff) -> anyhow::Result<()> {
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

			match client.connect(url.clone()).await {
				Ok(session) => {
					tracing::info!(%url, "connected");
					delay = backoff.initial;
					last_error = None;
					if let Ok(mut state) = state.write() {
						state.connects += 1;
					}
					let _ = session.closed().await;
					tracing::warn!(%url, "session closed, reconnecting");
					if let Ok(mut state) = state.write() {
						state.disconnects += 1;
					}
					retry_start = tokio::time::Instant::now();
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

	/// Poll for the next successful (re)connect after `since`.
	///
	/// Stays [`Poll::Pending`] once the loop has stopped; observe termination via
	/// [`poll_closed`](Self::poll_closed).
	pub fn poll_connected(&self, waiter: &conducer::Waiter, since: u64) -> Poll<u64> {
		match self.state.poll(waiter, |state| match state.connects {
			connects if connects > since => Poll::Ready(connects),
			_ => Poll::Pending,
		}) {
			Poll::Ready(Ok(connects)) => Poll::Ready(connects),
			// Loop stopped: let `closed` deliver the outcome instead.
			Poll::Ready(Err(_)) | Poll::Pending => Poll::Pending,
		}
	}

	/// Wait for the next successful (re)connect after `since`, returning the new epoch.
	///
	/// The epoch counts successful connects: 1 after the first connect, 2 after the
	/// first reconnect, and so on. Pass the previously returned value to wait for a
	/// newer connection, or 0 to wait for the first.
	///
	/// Never resolves once the reconnect loop has stopped, so pair it with
	/// [`closed`](Self::closed) in a `select!` to observe termination.
	pub async fn connected(&self, since: u64) -> u64 {
		conducer::wait(|waiter| self.poll_connected(waiter, since)).await
	}

	/// Poll for the next session drop after `since`.
	///
	/// Stays [`Poll::Pending`] once the loop has stopped; observe termination via
	/// [`poll_closed`](Self::poll_closed).
	pub fn poll_disconnected(&self, waiter: &conducer::Waiter, since: u64) -> Poll<u64> {
		match self.state.poll(waiter, |state| match state.disconnects {
			disconnects if disconnects > since => Poll::Ready(disconnects),
			_ => Poll::Pending,
		}) {
			Poll::Ready(Ok(disconnects)) => Poll::Ready(disconnects),
			Poll::Ready(Err(_)) | Poll::Pending => Poll::Pending,
		}
	}

	/// Wait for the next session drop after `since`, returning the new epoch.
	///
	/// A disconnect is not fatal: the loop keeps reconnecting. The epoch counts
	/// session drops. Pass the previously returned value to wait for a newer drop,
	/// or 0 to wait for the first.
	///
	/// Never resolves once the reconnect loop has stopped; [`closed`](Self::closed)
	/// is the only terminal signal.
	pub async fn disconnected(&self, since: u64) -> u64 {
		conducer::wait(|waiter| self.poll_disconnected(waiter, since)).await
	}

	/// Poll whether the reconnect loop has stopped.
	///
	/// Ready with `Err` if it permanently gave up (reconnect timeout exceeded),
	/// or `Ok(())` if stopped via [`close`](Self::close) or drop.
	pub fn poll_closed(&self, waiter: &conducer::Waiter) -> Poll<anyhow::Result<()>> {
		match self.state.poll_closed(waiter) {
			Poll::Ready(()) => Poll::Ready(self.outcome()),
			Poll::Pending => Poll::Pending,
		}
	}

	/// Wait until the reconnect loop stops.
	///
	/// Returns `Ok(())` if closed via [`close`](Self::close) or drop.
	/// Returns `Err` with the most recent connection error if the reconnect
	/// timeout was exceeded.
	pub async fn closed(&self) -> anyhow::Result<()> {
		conducer::wait(|waiter| self.poll_closed(waiter)).await
	}

	/// Read the terminal outcome; only meaningful once the channel has closed.
	fn outcome(&self) -> anyhow::Result<()> {
		match self.state.read().error.clone() {
			Some(err) => Err(anyhow::anyhow!("{err:#}")),
			None => Ok(()),
		}
	}

	/// Stop the background reconnect loop.
	pub fn close(self) {
		self.abort.abort();
	}
}

impl Drop for Reconnect {
	fn drop(&mut self) {
		self.abort.abort();
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
