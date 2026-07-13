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
	/// Resets after a stable connection (one that outlives the initial backoff), so a flapping
	/// session that reconnects then immediately drops still counts toward the timeout. Set to 0 for
	/// unlimited retries.
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

/// A snapshot of the live session's observable state, reported by [`Reconnect::changed`].
///
/// The reconnect loop owns the session, so a caller that needs the session's stats (a publisher
/// element surfacing them as properties, say) reads them from here rather than holding the session
/// itself. All fields reflect the *current* session and reset when it disconnects.
///
/// `#[non_exhaustive]`: read the fields you need; a future observable field won't be a breaking
/// change. Construct via [`Default`] when a placeholder is needed.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct Snapshot {
	/// Current connection status, or `None` before the first connect.
	pub status: Option<Status>,
	/// The negotiated MoQ version of the live session, or `None` when disconnected.
	pub version: Option<String>,
	/// The congestion controller's send estimate in bits/sec, `0` when disconnected or unavailable.
	pub send_bitrate: u64,
}

/// Shared reconnect state, observed by consumers through a [`kio`] channel.
///
/// The channel closing (all producers dropped) is the terminal signal; `error`
/// distinguishes a permanent give-up from a graceful close.
#[derive(Default)]
struct State {
	/// Current connection status, or `None` before the first connect.
	status: Option<Status>,
	/// The negotiated MoQ version of the live session, or `None` when disconnected.
	version: Option<String>,
	/// The live session's congestion-controller send estimate (bits/sec); `0` when disconnected
	/// or the backend has no estimate.
	send_bitrate: u64,
	/// Set when the reconnect loop permanently gives up (reconnect timeout exceeded).
	error: Option<Error>,
}

impl State {
	fn snapshot(&self) -> Snapshot {
		Snapshot {
			status: self.status,
			version: self.version.clone(),
			send_bitrate: self.send_bitrate,
		}
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
	/// The last snapshot returned by [`changed`](Self::changed), for change detection.
	last_snapshot: Option<Snapshot>,
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
			last_snapshot: None,
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
				Ok(session) => {
					tracing::info!(%url, "connected");
					if let Ok(mut state) = state.write() {
						state.status = Some(Status::Connected);
						state.version = Some(session.version().to_string());
						state.send_bitrate = 0;
					}

					let connected = tokio::time::Instant::now();
					// Wait for the session to close, forwarding its send-bandwidth estimate into the
					// shared state meanwhile so a consumer tracks the live stats across the connection.
					let closed = run_session(state, &session).await;
					if let Ok(mut state) = state.write() {
						state.status = Some(Status::Disconnected);
						state.version = None;
						state.send_bitrate = 0;
					}

					if connected.elapsed() >= backoff.initial {
						// Stayed up past the initial backoff: a healthy session. Reset the backoff
						// window so a one-off drop reconnects promptly.
						tracing::warn!(%url, "session closed, reconnecting");
						delay = backoff.initial;
						retry_start = tokio::time::Instant::now();
						last_error = None;
					} else {
						// Connected then dropped almost immediately (e.g. the server accepts then
						// resets). Treat it as a failed connection: keep the close reason so the
						// give-up timeout reports a real cause, and fall through to the shared backoff
						// sleep below so repeated flaps escalate instead of spinning the CPU.
						if let Err(err) = closed {
							let err = Error::from(err);
							tracing::warn!(%url, %err, "session severed immediately, retrying");
							last_error = Some(err);
						} else {
							tracing::warn!(%url, "session severed immediately, retrying");
						}
					}
				}
				Err(err) => {
					if err.is_auth() {
						return Err(err);
					}
					last_error = Some(err);
				}
			}

			tracing::warn!(%url, ?delay, "reconnecting after backoff");
			tokio::time::sleep(delay).await;
			delay = std::cmp::min(delay * backoff.multiplier, backoff.max);
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

	/// Poll for the next change to the live session's observable [`Snapshot`] since this handle last
	/// reported one.
	///
	/// `Ready(Ok(snapshot))` on any change (connect/disconnect, version, or send-bitrate),
	/// `Ready(Err)` once the loop has stopped, `Pending` otherwise.
	pub fn poll_changed(&mut self, waiter: &kio::Waiter) -> Poll<crate::Result<Snapshot>> {
		let last = self.last_snapshot.clone();
		let snapshot = match ready!(self.state.poll(waiter, |state| {
			let snapshot = state.snapshot();
			if Some(&snapshot) != last.as_ref() {
				Poll::Ready(snapshot)
			} else {
				Poll::Pending
			}
		})) {
			Ok(snapshot) => snapshot,
			Err(state) => return Poll::Ready(Err(terminal(&state))),
		};

		self.last_snapshot = Some(snapshot.clone());
		Poll::Ready(Ok(snapshot))
	}

	/// Wait until the live session's observable [`Snapshot`] changes from what this handle last
	/// reported — a connect/disconnect, a version change, or a send-bitrate update — and return the
	/// current snapshot. `Err` once the loop has stopped.
	///
	/// This is the stats-carrying counterpart to [`status`](Self::status): the reconnect loop owns
	/// the session, so a caller that needs the live session's version/send-bitrate reads them here
	/// instead of holding the session across reconnects.
	pub async fn changed(&mut self) -> crate::Result<Snapshot> {
		kio::wait(|waiter| self.poll_changed(waiter)).await
	}

	/// The negotiated MoQ version of the live session, or `None` when disconnected.
	pub fn version(&self) -> Option<String> {
		self.state.read().version.clone()
	}

	/// The live session's congestion-controller send estimate in bits/sec, `0` when disconnected or
	/// unavailable.
	pub fn send_bitrate(&self) -> u64 {
		self.state.read().send_bitrate
	}
}

/// Wait for `session` to close, forwarding its congestion-controller send estimate into `state`
/// meanwhile so a [`Reconnect`] consumer tracks the live send-bitrate. Returns the session's close
/// result (the reconnect loop uses it to distinguish a healthy drop from an immediate sever).
async fn run_session(state: &kio::Producer<State>, session: &moq_net::Session) -> Result<(), moq_net::Error> {
	// `None` when the QUIC backend has no bandwidth estimate; then that select arm parks forever.
	let mut send_bandwidth = session.send_bandwidth();
	let closed = session.closed();
	tokio::pin!(closed);

	loop {
		tokio::select! {
			result = &mut closed => return result,
			bitrate = async {
				match send_bandwidth.as_mut() {
					Some(bw) => bw.changed().await,
					None => std::future::pending::<Option<u64>>().await,
				}
			} => match bitrate {
				Some(rate) => {
					if let Ok(mut state) = state.write() {
						state.send_bitrate = rate;
					}
				}
				None => {
					// Estimate gone: report 0 (the documented "unavailable" value) and stop polling.
					if let Ok(mut state) = state.write() {
						state.send_bitrate = 0;
					}
					send_bandwidth = None;
				}
			},
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

	#[test]
	fn snapshot_reflects_state_and_detects_change() {
		// The observable surface `changed()`/`poll_changed()` forward is `State::snapshot()`, and the
		// change filter is snapshot inequality — so every observable field must round-trip and a
		// send-bitrate update alone must read as a distinct snapshot.
		let mut state = State::default();
		assert_eq!(state.snapshot(), Snapshot::default());

		state.status = Some(Status::Connected);
		state.version = Some("moq-lite-04".to_string());
		state.send_bitrate = 2_000_000;
		let connected = state.snapshot();
		assert_eq!(connected.status, Some(Status::Connected));
		assert_eq!(connected.version.as_deref(), Some("moq-lite-04"));
		assert_eq!(connected.send_bitrate, 2_000_000);

		// A send-bitrate change alone is a new snapshot (so a live estimate update wakes poll_changed).
		state.send_bitrate = 2_100_000;
		assert_ne!(state.snapshot(), connected);

		// Disconnect clears version + bitrate but keeps the (distinct) Disconnected status.
		state.status = Some(Status::Disconnected);
		state.version = None;
		state.send_bitrate = 0;
		let disconnected = state.snapshot();
		assert_eq!(disconnected.status, Some(Status::Disconnected));
		assert_eq!(disconnected.version, None);
		assert_eq!(disconnected.send_bitrate, 0);
		assert_ne!(disconnected, connected);
	}
}
