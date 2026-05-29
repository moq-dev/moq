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

/// A connection lifecycle transition reported by [`Reconnect::status`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
	/// A session connected (the first connect, or a reconnect after a drop).
	Connected,
	/// An established session dropped; a reconnect attempt follows.
	Disconnected,
}

/// Shared reconnect state, observed by consumers through a [`conducer`] channel.
///
/// The channel closing (all producers dropped) is the terminal signal; `error`
/// distinguishes a permanent give-up from a graceful close.
#[derive(Default)]
struct State {
	/// Number of connect/disconnect transitions so far. Connects and disconnects strictly
	/// alternate starting with a connect, so odd values are [`Status::Connected`] and even
	/// values are [`Status::Disconnected`].
	epoch: u64,
	/// Set when the reconnect loop permanently gives up (reconnect timeout exceeded).
	error: Option<Arc<anyhow::Error>>,
}

/// Handle to a background reconnect loop.
///
/// Spawns a tokio task that connects, waits for session close, then reconnects
/// with exponential backoff. [`status`](Self::status) reports each connect/disconnect
/// transition in order; [`closed`](Self::closed) waits for the loop to stop. Dropping
/// the handle aborts the background task.
pub struct Reconnect {
	abort: tokio::task::AbortHandle,
	state: conducer::Consumer<State>,
	/// Transitions already returned by [`status`](Self::status).
	cursor: u64,
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
			cursor: 0,
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
					// Odd epoch: connected.
					if let Ok(mut state) = state.write() {
						state.epoch += 1;
					}
					let _ = session.closed().await;
					tracing::warn!(%url, "session closed, reconnecting");
					// Even epoch: disconnected.
					if let Ok(mut state) = state.write() {
						state.epoch += 1;
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

	/// Wait for the next connect/disconnect transition, in order.
	///
	/// Connects and disconnects strictly alternate (connect, disconnect, connect, ...), so
	/// successive calls return `Connected`, `Disconnected`, `Connected`, ... Each call advances
	/// an internal cursor, so a caller never misses or reorders a transition even if the loop
	/// has already raced several reconnects ahead.
	///
	/// Returns `Err` once the loop has stopped: the give-up error if it exhausted its backoff
	/// timeout, or a generic "stopped" error after [`close`](Self::close) / drop.
	pub async fn status(&mut self) -> anyhow::Result<Status> {
		let since = self.cursor;

		// Clone the consumer so the borrow doesn't outlive the await, freeing us to bump the cursor.
		// Collapse the poll result to a bool inside the closure: a `Ref` in the poll output would
		// make the resulting future non-Send.
		let consumer = self.state.clone();
		let advanced = conducer::wait(|waiter| {
			match consumer.poll(waiter, |state| match state.epoch > since {
				true => Poll::Ready(()),
				false => Poll::Pending,
			}) {
				Poll::Ready(Ok(())) => Poll::Ready(true),
				Poll::Ready(Err(_)) => Poll::Ready(false),
				Poll::Pending => Poll::Pending,
			}
		})
		.await;

		if !advanced {
			// Channel closed: surface the terminal error (or a generic one for a graceful stop).
			return Err(self
				.outcome()
				.err()
				.unwrap_or_else(|| anyhow::anyhow!("reconnect stopped")));
		}

		// Position `since + 1`: odd is a connect, even is a disconnect.
		self.cursor = since + 1;
		Ok(match self.cursor % 2 {
			1 => Status::Connected,
			_ => Status::Disconnected,
		})
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
