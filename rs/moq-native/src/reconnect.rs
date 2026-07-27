use std::task::{Poll, ready};
use std::time::Duration;

use moq_net::Version;
use moq_net::bandwidth::{Consumer as BandwidthConsumer, Producer as BandwidthProducer};
use moq_net::kio;
use url::Url;

use crate::{Client, Error};

/// How the reconnect loop handles a GOAWAY-driven migration.
///
/// When the live session receives a GOAWAY carrying a redirect URI, the loop
/// dials the new target in parallel, keeps the old session alive for at most
/// `min(timeout, goaway_deadline)` so in-flight groups finish, then drops it.
/// Consumers see a brief [`Status::Migrating`] followed by [`Status::Connected`]
/// on the new session with no interruption to the origin.
#[derive(Clone, Debug, clap::Args, serde::Serialize, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
#[non_exhaustive]
pub struct Drain {
	/// Maximum time to keep the old connection alive while migrating.
	/// The effective deadline is `min(this, goaway_deadline)`. Zero means close
	/// the old session immediately once the new one is ready.
	#[arg(
		id = "drain-timeout",
		long = "drain-timeout",
		default_value = "10s",
		env = "MOQ_DRAIN_TIMEOUT",
		value_parser = humantime::parse_duration,
	)]
	#[serde(with = "humantime_serde")]
	pub timeout: Duration,
}

impl Default for Drain {
	fn default() -> Self {
		Self {
			timeout: Duration::from_secs(10),
		}
	}
}

/// Exponential backoff configuration for reconnection attempts.
#[derive(Clone, Debug, clap::Args, serde::Serialize, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
#[non_exhaustive]
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

impl Backoff {
	/// How long broadcasts fed by a reconnecting session should outlive a session
	/// drop (see [`moq_net::origin::Info::linger`]): slightly past the give-up
	/// [`timeout`](Self::timeout), so when the loop does give up its error surfaces
	/// before the broadcasts tear down. A zero timeout retries forever, so the
	/// broadcasts linger forever too.
	pub fn linger(&self) -> Duration {
		match self.timeout.is_zero() {
			true => Duration::MAX,
			false => self.timeout.saturating_add(Duration::from_secs(1)),
		}
	}
}

/// A connection lifecycle transition reported by [`Reconnect::status`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum Status {
	/// A session connected (the first connect, or a reconnect after a drop).
	Connected,
	/// An established session dropped; a reconnect attempt follows.
	Disconnected,
	/// A GOAWAY was received; the loop is dialing the redirect target while the
	/// old session drains in parallel. Consumers see no interruption to the
	/// origin; the next status is [`Connected`](Self::Connected) on the new
	/// session (success) or [`Disconnected`](Self::Disconnected) (both failed).
	Migrating,
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
	pub(crate) fn new(client: Client, url: Url, backoff: Backoff, drain: Drain) -> Self {
		let producer = kio::Producer::<State>::default();
		let state = producer.consume();

		// The loop feeds these across every reconnect, so a consumer's handle survives session churn
		// (unlike a session's own bandwidth consumer, which dies with the session).
		let send_bw = BandwidthProducer::new();
		let recv_bw = BandwidthProducer::new();
		let send_bandwidth = send_bw.consume();
		let recv_bandwidth = recv_bw.consume();

		let task = tokio::spawn(async move {
			if let Err(err) = Self::run(&producer, &send_bw, &recv_bw, client, url, backoff, drain).await {
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
		drain: Drain,
	) -> crate::Result<()> {
		let mut delay = backoff.initial;
		let mut retry_start = tokio::time::Instant::now();
		let mut last_error: Option<Error> = None;
		// The URL to dial next. Updated on a successful GOAWAY redirect so a
		// subsequent GOAWAY resolves against the latest target.
		let mut current_url = url;

		loop {
			if !backoff.timeout.is_zero() && retry_start.elapsed() > backoff.timeout {
				let timeout = backoff.timeout;
				let msg = match last_error {
					Some(err) => format!("reconnect timed out after {timeout:?}: {err}"),
					None => format!("reconnect timed out after {timeout:?}"),
				};
				return Err(Error::Reconnect(msg));
			}

			tracing::info!(url = %current_url, "connecting");

			match client.connect(current_url.clone()).await {
				Ok(session) => {
					tracing::info!(url = %current_url, "connected");
					if let Ok(mut s) = state.write() {
						s.status = Some(Status::Connected);
						s.version = Some(session.version());
						s.session = Some(session.clone());
					}

					let connected = tokio::time::Instant::now();

					// Run the session, watching for GOAWAY.
					let outcome = run_session_with_goaway(send_bw, recv_bw, &session).await;

					match outcome {
						SessionOutcome::Closed(closed) => {
							if let Ok(mut s) = state.write() {
								s.status = Some(Status::Disconnected);
								s.version = None;
								s.session = None;
							}
							let _ = send_bw.set(None);
							let _ = recv_bw.set(None);

							if connected.elapsed() >= backoff.initial {
								tracing::warn!(url = %current_url, "session closed, reconnecting");
								delay = backoff.initial;
								retry_start = tokio::time::Instant::now();
								last_error = None;
							} else if let Err(err) = closed {
								let err = Error::from(err);
								tracing::warn!(url = %current_url, %err, "session severed immediately, retrying");
								last_error = Some(err);
							} else {
								tracing::warn!(url = %current_url, "session severed immediately, retrying");
							}
						}
						SessionOutcome::Goaway(goaway) => {
							let redirect_url = resolve_redirect(&goaway.uri, &current_url);
							let effective_timeout = match goaway.timeout {
								Some(deadline) => drain.timeout.min(deadline),
								None => drain.timeout,
							};

							tracing::info!(
								redirect = %redirect_url,
								?effective_timeout,
								"GOAWAY received; migrating",
							);

							if let Ok(mut s) = state.write() {
								s.status = Some(Status::Migrating);
							}

							// Dial the redirect target.
							match client.connect(redirect_url.clone()).await {
								Ok(new_session) => {
									tracing::info!(%redirect_url, "redirect connected");

									// Drain the old session in the background.
									let old_session = session;
									tokio::spawn(async move {
										match tokio::time::timeout(
											effective_timeout,
											old_session.closed(),
										)
										.await
										{
											Ok(_) => {
												tracing::debug!(
													"old session drained cleanly after GOAWAY"
												);
											}
											Err(_elapsed) => {
												tracing::warn!(
													?effective_timeout,
													"old session did not drain in time; \
													 force-closing"
												);
												old_session
													.abort(moq_net::Error::GoawayTimeout);
											}
										}
									});

									// The redirect connected: update current_url so the next
									// GOAWAY or reconnect uses the new target.
									current_url = redirect_url;

									// Switch state to the new session.
									if let Ok(mut s) = state.write() {
										s.status = Some(Status::Connected);
										s.version = Some(new_session.version());
										s.session = Some(new_session.clone());
									}

									// Reset backoff on successful migration.
									delay = backoff.initial;
									retry_start = tokio::time::Instant::now();
									last_error = None;

									// Continue monitoring the new session. On its next
									// close or GOAWAY, the outer loop handles it.
									let new_connected = tokio::time::Instant::now();
									let new_outcome =
										run_session_with_goaway(send_bw, recv_bw, &new_session)
											.await;

									match new_outcome {
										SessionOutcome::Closed(closed) => {
											if let Ok(mut s) = state.write() {
												s.status = Some(Status::Disconnected);
												s.version = None;
												s.session = None;
											}
											let _ = send_bw.set(None);
											let _ = recv_bw.set(None);

											if new_connected.elapsed() >= backoff.initial {
												tracing::warn!(
													url = %current_url,
													"session closed, reconnecting"
												);
												delay = backoff.initial;
												retry_start = tokio::time::Instant::now();
												last_error = None;
											} else if let Err(err) = closed {
												let err = Error::from(err);
												tracing::warn!(
													url = %current_url, %err,
													"session severed immediately"
												);
												last_error = Some(err);
											} else {
												tracing::warn!(
													url = %current_url,
													"session severed immediately"
												);
											}
										}
										SessionOutcome::Goaway(next_goaway) => {
											// Nested GOAWAY: newest-wins. Update target
											// and restart the connect loop immediately.
											let next_redirect =
												resolve_redirect(&next_goaway.uri, &current_url);
											current_url = next_redirect;
											// No backoff for a GOAWAY redirect.
											delay = Duration::ZERO;
											continue;
										}
									}
								}
								Err(err) => {
									if err.is_auth() {
										return Err(err);
									}

									tracing::warn!(
										%redirect_url, %err,
										"redirect connect failed; falling back to backoff"
									);
									last_error = Some(err);

									if let Ok(mut s) = state.write() {
										s.status = Some(Status::Disconnected);
										s.version = None;
										s.session = None;
									}
									let _ = send_bw.set(None);
									let _ = recv_bw.set(None);

									// Drain the old session in the background.
									let old_session = session;
									tokio::spawn(async move {
										match tokio::time::timeout(
											effective_timeout,
											old_session.closed(),
										)
										.await
										{
											Ok(_) => {}
											Err(_) => {
												old_session
													.abort(moq_net::Error::GoawayTimeout);
											}
										}
									});
								}
							}
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

			if !delay.is_zero() {
				tracing::warn!(url = %current_url, ?delay, "reconnecting after backoff");
				tokio::time::sleep(delay).await;
			}
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

	/// A cloneable handle for reading the current connection's stats.
	///
	/// The handle keeps working across reconnects, reporting `None` between connections.
	pub fn stats(&self) -> ConnectionStatsReader {
		ConnectionStatsReader {
			state: self.state.clone(),
		}
	}
}

/// How a session ended from the reconnect loop's perspective.
enum SessionOutcome {
	/// The session closed (normally or with an error) without a GOAWAY.
	Closed(Result<(), moq_net::Error>),
	/// A GOAWAY was received; the session is still alive (draining).
	Goaway(moq_net::GoawayReceived),
}

/// Wait for `session` to either close or receive a GOAWAY, forwarding its
/// send/recv bandwidth estimates into the persistent producers meanwhile.
/// Returns the outcome so the caller can decide whether to migrate or reconnect.
async fn run_session_with_goaway(
	send_bw: &BandwidthProducer,
	recv_bw: &BandwidthProducer,
	session: &moq_net::Session,
) -> SessionOutcome {
	let mut send = session.send_bandwidth();
	let mut recv = session.recv_bandwidth();

	tokio::select! {
		err = session.closed() => {
			SessionOutcome::Closed(Err(err))
		}
		goaway = session.goaway() => {
			match goaway {
				Some(received) => SessionOutcome::Goaway(received),
				// Signal dropped without a GOAWAY: the session is closing.
				None => SessionOutcome::Closed(Err(session.closed().await)),
			}
		}
		// Drive bandwidth forwarding until one of the above resolves.
		_ = forward_bandwidth_loop(&mut send, &mut recv, send_bw, recv_bw) => {
			// This arm never completes (it loops forever), but it keeps the bandwidth
			// forwarding running in the background while we wait for close/goaway.
			unreachable!()
		}
	}
}

/// Forward bandwidth estimates from the session's consumers into the persistent
/// producers. Runs until cancelled (never returns on its own).
async fn forward_bandwidth_loop(
	send: &mut Option<BandwidthConsumer>,
	recv: &mut Option<BandwidthConsumer>,
	send_bw: &BandwidthProducer,
	recv_bw: &BandwidthProducer,
) {
	// This future never resolves: it keeps polling bandwidth changes until the
	// enclosing select! cancels it. The pending() below is unreachable but
	// satisfies the return type.
	loop {
		kio::wait(|waiter| {
			poll_forward(send, send_bw, waiter);
			poll_forward(recv, recv_bw, waiter);
			Poll::<()>::Pending
		})
		.await;
	}
}

/// Resolve a GOAWAY redirect URI into the URL to dial.
///
/// An empty URI means "reconnect to the same endpoint". A malformed or
/// security-downgrading redirect falls back to the original URL with a warning.
fn resolve_redirect(uri: &str, fallback: &Url) -> Url {
	if uri.is_empty() {
		return fallback.clone();
	}
	let Ok(parsed) = uri.parse::<Url>() else {
		tracing::warn!(uri, "malformed GOAWAY URI; falling back to original URL");
		return fallback.clone();
	};
	if scheme_security_tier(parsed.scheme()) < scheme_security_tier(fallback.scheme()) {
		tracing::warn!(
			redirect_scheme = parsed.scheme(),
			current_scheme = fallback.scheme(),
			uri,
			"GOAWAY redirect is a security downgrade; falling back to original URL",
		);
		return fallback.clone();
	}
	parsed
}

/// Security tier for the redirect no-downgrade guard: a peer-supplied redirect
/// must not silently downgrade an encrypted session to plaintext.
fn scheme_security_tier(scheme: &str) -> u8 {
	match scheme {
		"https" | "moqt" | "moql" | "wss" | "iroh" | "unix" => 2,
		"tcp" | "ws" | "http" => 1,
		_ => 0,
	}
}

/// Mirror `bw`'s live estimate into `out` for as long as it changes, dropping the source handle once
/// the session's producer is gone so we don't keep polling a dead arm. A `poll_*` step: on return,
/// `waiter` is registered for the next change (unless the source is gone). Seeding is implicit
/// (the first call forwards the current value if there is one).
///
/// A `None` estimate is forwarded but keeps the arm alive: the backend reporting nothing right now
/// isn't the same as the session ending, and the caller resets `out` to `None` on disconnect anyway.
fn poll_forward(bw: &mut Option<BandwidthConsumer>, out: &BandwidthProducer, waiter: &kio::Waiter) {
	loop {
		let Some(consumer) = bw.as_mut() else { return };
		let Poll::Ready(res) = consumer.poll_changed(waiter) else {
			return;
		};
		match res {
			Ok(rate) => {
				let _ = out.set(rate);
			}
			Err(_) => {
				*bw = None;
				return;
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

	/// The linger outlives the give-up timeout (so the reconnect error surfaces
	/// first), and an unlimited-retry timeout lingers forever.
	#[test]
	fn test_backoff_linger() {
		let backoff = Backoff::default();
		assert_eq!(backoff.linger(), backoff.timeout + Duration::from_secs(1));

		let unlimited = Backoff {
			timeout: Duration::ZERO,
			..Backoff::default()
		};
		assert_eq!(unlimited.linger(), Duration::MAX);
	}

	#[test]
	fn poll_forward_mirrors_until_the_source_closes() {
		let src = BandwidthProducer::new();
		let out = BandwidthProducer::new();
		let out_rx = out.consume();
		let waiter = kio::Waiter::noop();

		// No estimate yet: nothing forwarded, source retained.
		let mut bw = Some(src.consume());
		poll_forward(&mut bw, &out, &waiter);
		assert_eq!(out_rx.peek(), None);
		assert!(bw.is_some());

		// A value is mirrored through.
		src.set(Some(3_000)).unwrap();
		poll_forward(&mut bw, &out, &waiter);
		assert_eq!(out_rx.peek(), Some(3_000));

		// The estimate becoming unavailable is mirrored, but the arm stays: the
		// backend reporting nothing right now is not the session ending.
		src.set(None).unwrap();
		poll_forward(&mut bw, &out, &waiter);
		assert_eq!(out_rx.peek(), None);
		assert!(bw.is_some());

		// So a later value on the same live session still gets through. Dropping the
		// arm on the `None` above would have stranded the estimate at `None` for the
		// rest of the session.
		src.set(Some(9_000)).unwrap();
		poll_forward(&mut bw, &out, &waiter);
		assert_eq!(out_rx.peek(), Some(9_000));

		// Closing the source is what retires the arm, so we stop polling a dead one.
		src.abort(moq_net::Error::Cancel).unwrap();
		poll_forward(&mut bw, &out, &waiter);
		assert!(bw.is_none());
	}

	#[test]
	fn test_drain_default() {
		let drain = Drain::default();
		assert_eq!(drain.timeout, Duration::from_secs(10));
	}

	#[test]
	fn resolve_redirect_empty_uri_returns_fallback() {
		let fallback = Url::parse("https://relay.example.com/anon").unwrap();
		assert_eq!(resolve_redirect("", &fallback), fallback);
	}

	#[test]
	fn resolve_redirect_valid_uri() {
		let fallback = Url::parse("https://relay.example.com/anon").unwrap();
		let result = resolve_redirect("https://other.example.com/path", &fallback);
		assert_eq!(result.as_str(), "https://other.example.com/path");
	}

	#[test]
	fn resolve_redirect_malformed_uri_returns_fallback() {
		let fallback = Url::parse("https://relay.example.com/anon").unwrap();
		assert_eq!(resolve_redirect("not a url at all!", &fallback), fallback);
	}

	#[test]
	fn resolve_redirect_rejects_security_downgrade() {
		let fallback = Url::parse("https://relay.example.com/anon").unwrap();
		// http is a downgrade from https.
		assert_eq!(
			resolve_redirect("http://insecure.example.com/path", &fallback),
			fallback
		);
	}

	#[test]
	fn resolve_redirect_allows_same_tier() {
		let fallback = Url::parse("https://relay.example.com/anon").unwrap();
		// moqt is same tier as https.
		let result = resolve_redirect("moqt://other.example.com:4443", &fallback);
		assert_eq!(result.as_str(), "moqt://other.example.com:4443");
	}

	#[test]
	fn resolve_redirect_allows_upgrade() {
		let fallback = Url::parse("http://relay.example.com/anon").unwrap();
		// https is an upgrade from http.
		let result = resolve_redirect("https://secure.example.com/path", &fallback);
		assert_eq!(result.as_str(), "https://secure.example.com/path");
	}

	#[test]
	fn scheme_security_tiers() {
		assert_eq!(scheme_security_tier("https"), 2);
		assert_eq!(scheme_security_tier("moqt"), 2);
		assert_eq!(scheme_security_tier("moql"), 2);
		assert_eq!(scheme_security_tier("wss"), 2);
		assert_eq!(scheme_security_tier("iroh"), 2);
		assert_eq!(scheme_security_tier("unix"), 2);
		assert_eq!(scheme_security_tier("tcp"), 1);
		assert_eq!(scheme_security_tier("ws"), 1);
		assert_eq!(scheme_security_tier("http"), 1);
		assert_eq!(scheme_security_tier("ftp"), 0);
	}
}
