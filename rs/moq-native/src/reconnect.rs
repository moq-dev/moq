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
	/// Resets after each successful connection. Omit for unlimited retries.
	#[arg(
		id = "backoff-timeout",
		long,
		env = "MOQ_BACKOFF_TIMEOUT",
		value_parser = humantime::parse_duration,
	)]
	#[serde(default, with = "humantime_serde", skip_serializing_if = "Option::is_none")]
	pub timeout: Option<Duration>,
}

impl Default for Backoff {
	fn default() -> Self {
		Self {
			initial: Duration::from_secs(1),
			multiplier: 2,
			max: Duration::from_secs(30),
			timeout: None,
		}
	}
}

/// Handle to a background reconnect loop.
///
/// Spawns a tokio task that connects, waits for session close, then reconnects
/// with exponential backoff. Dropping the handle aborts the background task.
pub struct Reconnect {
	abort: tokio::task::AbortHandle,
	closed_rx: tokio::sync::watch::Receiver<bool>,
}

impl Reconnect {
	pub(crate) fn new(client: Client, url: Url, backoff: Backoff) -> Self {
		let (closed_tx, closed_rx) = tokio::sync::watch::channel(false);
		let task = tokio::spawn(async move {
			if let Err(err) = Self::run(client, url, backoff).await {
				tracing::error!(%err, "reconnect loop exited");
				let _ = closed_tx.send(true);
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

		loop {
			if let Some(timeout) = backoff.timeout {
				if retry_start.elapsed() > timeout {
					anyhow::bail!("reconnect timed out after {timeout:?}");
				}
			}

			tracing::info!(%url, "connecting");

			match client.connect(url.clone()).await {
				Ok(session) => {
					tracing::info!(%url, "connected");
					delay = backoff.initial;
					let _ = session.closed().await;
					tracing::warn!(%url, "session closed, reconnecting");
					retry_start = tokio::time::Instant::now();
				}
				Err(err) => {
					tracing::warn!(%url, %err, ?delay, "connection failed, retrying");
					tokio::time::sleep(delay).await;
					delay = std::cmp::min(delay * backoff.multiplier, backoff.max);
				}
			}
		}
	}

	/// Wait until the reconnect loop stops.
	///
	/// Returns `Ok(())` if closed via [`close`](Self::close) or drop.
	/// Returns `Err` if the reconnect timeout was exceeded.
	pub async fn closed(&self) -> anyhow::Result<()> {
		let mut rx = self.closed_rx.clone();
		match rx.wait_for(|&v| v).await {
			Ok(_) => anyhow::bail!("reconnect timed out"),
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
		assert_eq!(backoff.timeout, None);
	}
}
