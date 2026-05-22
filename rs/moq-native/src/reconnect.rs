use std::sync::Arc;
use std::time::Duration;

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

/// Handle to a background reconnect loop.
///
/// Spawns a tokio task that connects, waits for session close, then reconnects
/// with exponential backoff. Dropping the handle aborts the background task.
pub struct Reconnect {
	abort: tokio::task::AbortHandle,
	closed_rx: tokio::sync::watch::Receiver<Option<Arc<anyhow::Error>>>,
}

impl Reconnect {
	pub(crate) fn new(client: Client, url: Url, backoff: Backoff) -> Self {
		let (closed_tx, closed_rx) = tokio::sync::watch::channel(None::<Arc<anyhow::Error>>);
		let task = tokio::spawn(async move {
			if let Err(err) = Self::run(client, url, backoff).await {
				tracing::error!(err = %format!("{err:#}"), "reconnect loop exited");
				let _ = closed_tx.send(Some(Arc::new(err)));
			}
		});
		Self {
			abort: task.abort_handle(),
			closed_rx,
		}
	}

	async fn run(client: Client, url: Url, backoff: Backoff) -> anyhow::Result<()> {
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
					let _ = session.closed().await;
					tracing::warn!(%url, "session closed, reconnecting");
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

	/// Wait until the reconnect loop stops.
	///
	/// Returns `Ok(())` if closed via [`close`](Self::close) or drop.
	/// Returns `Err` with the most recent connection error if the reconnect
	/// timeout was exceeded.
	pub async fn closed(&self) -> anyhow::Result<()> {
		let mut rx = self.closed_rx.clone();
		match rx.wait_for(|v| v.is_some()).await {
			Ok(v) => {
				let err = Arc::clone(v.as_ref().expect("predicate matched Some"));
				Err(anyhow::anyhow!("{err:#}"))
			}
			Err(_) => Ok(()),
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
