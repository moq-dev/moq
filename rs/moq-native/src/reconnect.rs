use std::task::{Poll, ready};
use std::time::Duration;

use moq_net::kio;
use moq_net::{BandwidthConsumer, BandwidthProducer, Version};
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

/// Shared reconnect state, observed by consumers through a [`kio`] channel.
///
/// The channel closing (all producers dropped) is the terminal signal; `error`
/// distinguishes a permanent give-up from a graceful close.
#[derive(Default)]
struct State {
	/// Current connection status, or `None` before the first connect.
	status: Option<Status>,
	/// The negotiated MoQ version of the live session, or `None` when disconnected.
	version: Option<Version>,
	/// Set when the reconnect loop permanently gives up (reconnect timeout exceeded).
	error: Option<Error>,
}

/// Handle to a background reconnect loop.
///
/// Spawns a tokio task that connects, waits for session close, then reconnects with exponential
/// backoff. The read surface mirrors [`moq_net::Session`] so a caller can treat it like a session
/// that transparently reconnects: [`version`](Self::version), [`send_bandwidth`](Self::send_bandwidth),
/// and [`recv_bandwidth`](Self::recv_bandwidth) track the live session and reset while disconnected.
/// The extra toggle a plain session doesn't have is the connection lifecycle: [`connected`](Self::connected)
/// reads it synchronously and [`status`](Self::status) waits for the next change. [`closed`](Self::closed)
/// waits for the loop to stop. Dropping the handle aborts the background task.
pub struct Reconnect {
	abort: tokio::task::AbortHandle,
	state: kio::Consumer<State>,
	/// Persistent send-bitrate estimate, fed by the loop from each live session.
	send_bandwidth: BandwidthConsumer,
	/// Persistent recv-bitrate estimate, fed by the loop from each live session.
	recv_bandwidth: BandwidthConsumer,
	/// The last status returned by [`status`](Self::status), for change detection.
	last_reported: Option<Status>,
}

impl Reconnect {
	pub(crate) fn new(client: Client, url: Url, backoff: Backoff) -> Self {
		let producer = kio::Producer::<State>::default();
		let state = producer.consume();

		// The loop feeds these across every reconnect, so a consumer's handle survives session churn
		// (unlike a session's own bandwidth consumer, which dies with the session).
		let send_bw = BandwidthProducer::new();
		let recv_bw = BandwidthProducer::new();
		let send_bandwidth = send_bw.consume();
		let recv_bandwidth = recv_bw.consume();

		let task = tokio::spawn(async move {
			if let Err(err) = Self::run(&producer, &send_bw, &recv_bw, client, url, backoff).await {
				tracing::error!(%err, "reconnect loop exited");
				if let Ok(mut state) = producer.write() {
					state.error = Some(err);
				}
			}
			// Dropping the producers here closes the channels, signaling consumers.
		});
		Self {
			abort: task.abort_handle(),
			state,
			send_bandwidth,
			recv_bandwidth,
			last_reported: None,
		}
	}

	async fn run(
		state: &kio::Producer<State>,
		send_bw: &BandwidthProducer,
		recv_bw: &BandwidthProducer,
		client: Client,
		url: Url,
		backoff: Backoff,
	) -> crate::Result<()> {
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
						state.version = Some(session.version());
					}

					let connected = tokio::time::Instant::now();
					// Wait for the session to close, forwarding its bandwidth estimates into the
					// persistent producers meanwhile so consumers track the live stats across the connection.
					let closed = run_session(send_bw, recv_bw, &session).await;
					if let Ok(mut state) = state.write() {
						state.status = Some(Status::Disconnected);
						state.version = None;
					}
					// The estimates belonged to the now-closed session; reset until the next connect.
					let _ = send_bw.set(None);
					let _ = recv_bw.set(None);

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

	/// Whether a session is currently connected.
	///
	/// The synchronous read behind [`status`](Self::status), for callers that just want the current
	/// state rather than the next change.
	pub fn connected(&self) -> bool {
		self.state.read().status == Some(Status::Connected)
	}

	/// The negotiated MoQ version of the live session, or `None` while disconnected.
	///
	/// The [`moq_net::Session::version`] counterpart; `Option` because a reconnecting handle can be
	/// between sessions.
	pub fn version(&self) -> Option<Version> {
		self.state.read().version
	}

	/// A consumer for the live session's estimated send bitrate, mirroring
	/// [`moq_net::Session::send_bandwidth`].
	///
	/// Unlike the session's, this handle is persistent: the reconnect loop forwards each session's
	/// estimate into it, so it survives reconnects. Its value is `None` while disconnected or when the
	/// backend has no estimate.
	pub fn send_bandwidth(&self) -> BandwidthConsumer {
		self.send_bandwidth.clone()
	}

	/// A consumer for the live session's estimated receive bitrate, mirroring
	/// [`moq_net::Session::recv_bandwidth`]. Persistent across reconnects like
	/// [`send_bandwidth`](Self::send_bandwidth); `None` while disconnected or unavailable.
	pub fn recv_bandwidth(&self) -> BandwidthConsumer {
		self.recv_bandwidth.clone()
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
}

/// Wait for `session` to close, forwarding its send/recv bandwidth estimates into the persistent
/// producers meanwhile so [`Reconnect`] consumers track the live estimates across the connection.
/// Returns the session's close result (the loop uses it to distinguish a healthy drop from an
/// immediate sever).
async fn run_session(
	send_bw: &BandwidthProducer,
	recv_bw: &BandwidthProducer,
	session: &moq_net::Session,
) -> Result<(), moq_net::Error> {
	// `None` when the backend/version has no estimate; then that select arm parks forever.
	let mut send = session.send_bandwidth();
	let mut recv = session.recv_bandwidth();

	// Seed the current estimates so a consumer reads them without waiting for the first change.
	let _ = send_bw.set(send.as_ref().and_then(|bw| bw.peek()));
	let _ = recv_bw.set(recv.as_ref().and_then(|bw| bw.peek()));

	let closed = session.closed();
	tokio::pin!(closed);

	loop {
		tokio::select! {
			result = &mut closed => return result,
			rate = async {
				match send.as_mut() {
					Some(bw) => bw.changed().await,
					None => std::future::pending::<Option<u64>>().await,
				}
			} => {
				let _ = send_bw.set(rate);
				// Estimate gone (or the session's producer dropped): stop polling this arm.
				if rate.is_none() {
					send = None;
				}
			}
			rate = async {
				match recv.as_mut() {
					Some(bw) => bw.changed().await,
					None => std::future::pending::<Option<u64>>().await,
				}
			} => {
				let _ = recv_bw.set(rate);
				if rate.is_none() {
					recv = None;
				}
			}
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
