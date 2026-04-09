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
}

impl Default for Backoff {
	fn default() -> Self {
		Self {
			initial: Duration::from_secs(1),
			multiplier: 2,
			max: Duration::from_secs(30),
		}
	}
}

/// Handle to a background reconnect loop.
///
/// Spawns a tokio task that connects, waits for session close, then reconnects
/// with exponential backoff. Dropping the handle aborts the background task.
pub struct Reconnect {
	handle: tokio::task::AbortHandle,
}

impl Reconnect {
	pub(crate) fn new(client: Client, url: Url, backoff: Backoff) -> Self {
		let task = tokio::spawn(Self::run(client, url, backoff));
		Self {
			handle: task.abort_handle(),
		}
	}

	async fn run(client: Client, url: Url, backoff: Backoff) {
		let mut delay = backoff.initial;

		loop {
			tracing::info!(%url, "connecting");

			match client.connect(url.clone()).await {
				Ok(session) => {
					tracing::info!(%url, "connected");
					delay = backoff.initial;
					let _ = session.closed().await;
					tracing::warn!(%url, "session closed, reconnecting");
				}
				Err(err) => {
					tracing::warn!(%url, %err, ?delay, "connection failed, retrying");
					tokio::time::sleep(delay).await;
					delay = std::cmp::min(delay * backoff.multiplier, backoff.max);
				}
			}
		}
	}

	/// Stop the background reconnect loop.
	pub fn close(self) {
		self.handle.abort();
	}
}

impl Drop for Reconnect {
	fn drop(&mut self) {
		self.handle.abort();
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
	}
}
