//! The reconnect loop around a session, and the [`Monitor`] that observes it.
//!
//! The policy (the connect deadline, the address walk, backoff, and GOAWAY handling) is
//! [`moq_net::Reconnect`]'s; this module dials what it asks for, runs it on a task, and
//! publishes what it does.

use std::task::{Poll, ready};
use std::time::Duration;

use moq_net::Version;
use moq_net::bandwidth::{Consumer as BandwidthConsumer, Producer as BandwidthProducer};
use moq_net::kio;
use moq_net::reconnect::{Action, Verdict};

use crate::abort::AbortOnDrop;
use crate::connect::Endpoint;
use crate::{Addrs, Client, Error};

pub use moq_net::reconnect::Restart;

/// How a failed dial bears on the rest of the policy.
///
/// A status the peer actually sent is its answer, not this address's, so unless it invites
/// another attempt it settles the walk. An auth rejection arrives as [`Error::Connect`] with
/// its status already consumed, so it is checked alongside the statuses.
fn verdict(err: &Error) -> Verdict {
	if let Error::MoqNet(err) = err {
		return Verdict::from(err);
	}
	let terminal = err.is_terminal();
	if err.is_auth() && terminal {
		return Verdict::Refused;
	}
	let settled = err.is_auth()
		|| err
			.status()
			.is_some_and(|status| !crate::error::status_retryable(status));
	match (settled, terminal) {
		(true, true) => Verdict::Fatal,
		(true, false) => Verdict::Settled,
		(false, true) => Verdict::Unusable,
		(false, false) => Verdict::Retry,
	}
}

/// Exponential backoff configuration for reconnection attempts.
///
/// Three knobs, each with one job: `initial` and `multiplier` set the wait before
/// each retry, `max` caps it, and `healthy` decides which sessions count as failed
/// attempts. See [`moq_net::reconnect::Backoff`], which this resolves to, for how
/// the policy applies them.
#[derive(Clone, Debug, usage_rs::Args, serde::Serialize, serde::Deserialize)]
#[usage(unknown_flags = "error", args_override_self = false)]
#[serde(default, deny_unknown_fields)]
#[non_exhaustive]
pub struct Backoff {
	/// Delay before the first reconnect attempt. Defaults to 1s; zero retries at once.
	///
	/// Later delays grow from it, so zero turns retry waiting off entirely: an immediate
	/// close still counts as a failed attempt and spends the timeout.
	#[usage(
		name = "connect-backoff-initial",
		long,
		env = "MOQ_CONNECT_BACKOFF_INITIAL",
		setting = "connect.backoff.initial"
	)]
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub initial: Option<crate::cli::Duration>,

	/// Multiplier applied to delay after each failure. Defaults to 2; 0 or 1 keeps it constant.
	#[usage(
		name = "connect-backoff-multiplier",
		long,
		env = "MOQ_CONNECT_BACKOFF_MULTIPLIER",
		default = "2",
		setting = "connect.backoff.multiplier"
	)]
	pub multiplier: u32,

	/// Cap on the delay between reconnect attempts. Defaults to 5s; zero leaves it uncapped.
	///
	/// A cap below `initial` caps the first delay too.
	#[usage(
		name = "connect-backoff-max",
		long,
		env = "MOQ_CONNECT_BACKOFF_MAX",
		setting = "connect.backoff.max"
	)]
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub max: Option<crate::cli::Duration>,

	/// How long a session must stay up to count as healthy. Defaults to 1s; zero is refused.
	///
	/// A healthy session's end resets the delay and the timeout. One that ends sooner is a
	/// failed attempt, so a peer that accepts and then drops every session still escalates
	/// the delay and exhausts the timeout.
	#[usage(
		name = "connect-backoff-healthy",
		long,
		env = "MOQ_CONNECT_BACKOFF_HEALTHY",
		setting = "connect.backoff.healthy"
	)]
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub healthy: Option<crate::cli::Duration>,

	/// Maximum time to spend retrying before giving up. Defaults to 10s; zero retries forever.
	///
	/// No new walk of the peer's addresses starts once it has passed, but one under way finishes:
	/// each candidate but the last gets up to 5s, and the last runs to `--connect-timeout`,
	/// so a handshake slower than this still lands. Giving up can therefore take up to one whole walk longer.
	///
	/// Resets after a healthy session outside a redirect loop, so a flapping session that
	/// reconnects then immediately drops still counts toward the timeout.
	#[usage(
		name = "connect-backoff-timeout",
		long,
		env = "MOQ_CONNECT_BACKOFF_TIMEOUT",
		setting = "connect.backoff.timeout"
	)]
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub timeout: Option<crate::cli::Duration>,
	#[usage(
		name = "backoff-initial",
		long = "backoff-initial",
		env = "MOQ_BACKOFF_INITIAL",
		hide = true
	)]
	#[serde(skip)]
	legacy_initial: Option<crate::cli::Duration>,

	#[usage(
		name = "backoff-multiplier",
		long = "backoff-multiplier",
		env = "MOQ_BACKOFF_MULTIPLIER",
		hide = true
	)]
	#[serde(skip)]
	legacy_multiplier: Option<u32>,

	#[usage(name = "backoff-max", long = "backoff-max", env = "MOQ_BACKOFF_MAX", hide = true)]
	#[serde(skip)]
	legacy_max: Option<crate::cli::Duration>,

	#[usage(
		name = "backoff-timeout",
		long = "backoff-timeout",
		env = "MOQ_BACKOFF_TIMEOUT",
		hide = true
	)]
	#[serde(skip)]
	legacy_timeout: Option<crate::cli::Duration>,
}

impl Default for Backoff {
	fn default() -> Self {
		Self {
			initial: None,
			multiplier: moq_net::reconnect::Backoff::default().multiplier,
			max: None,
			healthy: None,
			timeout: None,
			legacy_initial: None,
			legacy_multiplier: None,
			legacy_max: None,
			legacy_timeout: None,
		}
	}
}

impl Backoff {
	/// The configured initial retry delay, or its default; zero retries at once.
	pub fn initial(&self) -> Duration {
		self.resolve().initial
	}

	/// The configured retry-delay cap, or its default; zero leaves the delay uncapped.
	pub fn max(&self) -> Duration {
		self.resolve().max
	}

	/// The retry deadline window, with zero meaning unlimited and unset using the default.
	pub fn timeout(&self) -> Duration {
		self.resolve().timeout
	}

	/// The policy these knobs configure, with each unset one at its default.
	pub(crate) fn resolve(&self) -> moq_net::reconnect::Backoff {
		let mut backoff = moq_net::reconnect::Backoff::default();
		backoff.multiplier = self.multiplier;
		let knobs = [
			(&mut backoff.initial, self.initial),
			(&mut backoff.max, self.max),
			(&mut backoff.healthy, self.healthy),
			(&mut backoff.timeout, self.timeout),
		];
		for (knob, set) in knobs {
			if let Some(set) = set {
				*knob = set.into();
			}
		}
		backoff
	}

	/// Refuse a zero healthy threshold, which would count every session as healthy and
	/// reset the timeout forever under a peer that drops each one on sight.
	#[cfg(feature = "_transport")]
	pub(crate) fn validate(&self) -> crate::Result<()> {
		if self.resolve().healthy.is_zero() {
			return Err(Error::BackoffHealthy);
		}
		Ok(())
	}

	pub(crate) fn deprecated(&self) -> crate::cli::Deprecated {
		let mut found = crate::cli::Deprecated::default();
		if self.legacy_initial.is_some() {
			found.flag(
				"--backoff-initial",
				Some("MOQ_BACKOFF_INITIAL"),
				"--connect-backoff-initial / MOQ_CONNECT_BACKOFF_INITIAL",
			);
		}
		if self.legacy_multiplier.is_some() {
			found.flag(
				"--backoff-multiplier",
				Some("MOQ_BACKOFF_MULTIPLIER"),
				"--connect-backoff-multiplier / MOQ_CONNECT_BACKOFF_MULTIPLIER",
			);
		}
		if self.legacy_max.is_some() {
			found.flag(
				"--backoff-max",
				Some("MOQ_BACKOFF_MAX"),
				"--connect-backoff-max / MOQ_CONNECT_BACKOFF_MAX",
			);
		}
		if self.legacy_timeout.is_some() {
			found.flag(
				"--backoff-timeout",
				Some("MOQ_BACKOFF_TIMEOUT"),
				"--connect-backoff-timeout / MOQ_CONNECT_BACKOFF_TIMEOUT",
			);
		}
		found
	}
}

/// A connection lifecycle transition reported by [`Connection::status`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum Status {
	/// A session connected (the first connect, or a reconnect after a drop).
	Connected,
	/// An established session dropped; a reconnect attempt follows.
	Disconnected,
	/// The peer sent a GOAWAY; a replacement session is coming up while the old one
	/// keeps serving.
	Migrating,
}

/// What to do with the URI a peer names in its GOAWAY, as `--connect-goaway-redirect`
/// spells it. See [`moq_net::reconnect::Redirect`] for what each one risks.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, usage_rs::ValueEnum, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
#[non_exhaustive]
pub enum Redirect {
	/// Follow any URI that does not downgrade the scheme, letting the peer name the
	/// host as well as the port. Use it only with an upstream trusted with the local
	/// network and with the tokens on [`Connection::auth`].
	Follow,
	/// Follow only when the host matches the configured URL, so a peer can move us
	/// between ports or schemes but not to another host. The default.
	#[default]
	SameHost,
	/// Ignore the URI and retry the current address list.
	Ignore,
}

impl From<Redirect> for moq_net::reconnect::Redirect {
	fn from(redirect: Redirect) -> Self {
		match redirect {
			Redirect::Follow => Self::Follow,
			Redirect::SameHost => Self::SameHost,
			Redirect::Ignore => Self::Ignore,
		}
	}
}

/// How a reconnect loop reacts to a peer's GOAWAY.
#[derive(Clone, Debug, usage_rs::Args, serde::Serialize, serde::Deserialize)]
#[usage(unknown_flags = "error", args_override_self = false)]
#[serde(default, deny_unknown_fields)]
#[non_exhaustive]
pub struct Goaway {
	/// What to do with the URI a peer names in its GOAWAY. `same-host` (the
	/// default) lets it move us between ports and schemes on the host we already
	/// chose, `follow` also lets it name the host, and `ignore` redials the
	/// current address list.
	#[usage(
		name = "connect-goaway-redirect",
		long,
		env = "MOQ_CONNECT_GOAWAY_REDIRECT",
		value_enum,
		default = "same-host",
		setting = "connect.goaway.redirect"
	)]
	pub redirect: Redirect,

	/// The longest the old session keeps serving after a GOAWAY, e.g. "10s" or
	/// "500ms". It closes sooner, once its replacement is up and it carries no
	/// subscription or fetch. A GOAWAY naming a shorter deadline wins, since the peer
	/// force-closes at its own deadline regardless, but a longer one does not extend
	/// it. Defaults to 10 seconds.
	/// `None` uses the default; zero disables this duration.
	#[usage(
		name = "connect-goaway-handover",
		long,
		env = "MOQ_CONNECT_GOAWAY_HANDOVER",
		setting = "connect.goaway.handover"
	)]
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub handover: Option<crate::cli::Duration>,
	#[usage(
		name = "goaway-redirect",
		long = "goaway-redirect",
		env = "MOQ_GOAWAY_REDIRECT",
		value_enum,
		hide = true
	)]
	#[serde(skip)]
	legacy_redirect: Option<Redirect>,

	#[usage(
		name = "goaway-handover",
		long = "goaway-handover",
		env = "MOQ_GOAWAY_HANDOVER",
		hide = true
	)]
	#[serde(skip)]
	legacy_handover: Option<crate::cli::Duration>,
}

impl Default for Goaway {
	fn default() -> Self {
		Self {
			redirect: Redirect::SameHost,
			handover: None,
			legacy_redirect: None,
			legacy_handover: None,
		}
	}
}

impl Goaway {
	pub(crate) fn deprecated(&self) -> crate::cli::Deprecated {
		let mut found = crate::cli::Deprecated::default();
		if self.legacy_redirect.is_some() {
			found.flag(
				"--goaway-redirect",
				Some("MOQ_GOAWAY_REDIRECT"),
				"--connect-goaway-redirect / MOQ_CONNECT_GOAWAY_REDIRECT",
			);
		}
		if self.legacy_handover.is_some() {
			found.flag(
				"--goaway-handover",
				Some("MOQ_GOAWAY_HANDOVER"),
				"--connect-goaway-handover / MOQ_CONNECT_GOAWAY_HANDOVER",
			);
		}
		found
	}

	/// The policy these knobs configure, with each unset one at its default.
	pub(crate) fn resolve(&self) -> moq_net::reconnect::Goaway {
		let mut goaway = moq_net::reconnect::Goaway::default();
		goaway.redirect = self.redirect.into();
		if let Some(handover) = self.handover {
			goaway.handover = handover.into();
		}
		goaway
	}
}
/// Shared reconnect state, observed by consumers through a [`kio`] channel.
///
/// The channel closing (all producers dropped) is the terminal signal; `error`
/// distinguishes a permanent give-up from a graceful close.
#[derive(Default)]
struct State {
	/// Advances on every lifecycle update, including identical failed dials.
	revision: u64,
	/// The loop has finished and cannot publish another state.
	ended: bool,
	/// Current connection status, or `None` before the first connect.
	status: Option<Status>,
	/// How many sessions have been live on this handle, counting the current one.
	/// Advanced by `Shared::connected` with the same write that publishes `status`,
	/// so a reader can't see `Connected` without its count.
	connects: u64,
	last_connected: Option<kio::clock::Instant>,
	/// Cumulative connects and disconnects, bumped by the reconnect loop itself so a session that
	/// connects and drops before a consumer polls still counts.
	presence: moq_net::stats::Presence,
	/// The negotiated MoQ version of the live session, or `None` when disconnected.
	version: Option<Version>,
	/// What the live session runs on, or `None` when disconnected.
	transport: Option<crate::Transport>,
	/// Set when the reconnect loop permanently gives up (reconnect timeout exceeded).
	error: Option<Error>,
	/// Why nothing is live while the loop retries: the last session's close reason or
	/// the latest failed dial. Cleared by the next connect.
	failure: Option<Error>,
	/// The currently-connected session, or `None` while reconnecting. Read by
	/// [`Monitor`] to snapshot live connection stats.
	session: Option<moq_net::Session>,
	/// The policy's draining predecessor and its handover deadline, so
	/// [`Connection::close`] drains it too without overstaying that window.
	predecessor: Option<(moq_net::Session, kio::clock::Instant)>,
}

/// The producer side of everything a [`Connection`] handle can observe.
///
/// These three travel together through the loop, and every lifecycle transition
/// touches more than one of them: leaving a session live in [`State`] while the
/// bandwidth estimates reset (or the reverse) is how a handle ends up reporting a
/// session that is already gone. Keeping them behind one value means a transition
/// is one call rather than a block to remember.
struct Shared {
	state: kio::Producer<State>,
	send_bw: BandwidthProducer,
	recv_bw: BandwidthProducer,
	closed: CloseGuard,
	/// The auth handle every session is built on, ended after the loop and any close drain.
	auth: moq_net::auth::Handle,
	/// The tokens every dial presents, on `auth`'s sessions.
	tokens: moq_net::auth::Tokens,
}

impl Shared {
	/// A session is live and serving.
	fn connected(&self, session: &moq_net::Session, transport: crate::Transport) {
		// Held across the publish: see [`CloseGuard`]. A close that already ran is
		// honored here rather than leaving this session parked in the final state.
		let closed = self.closed.lock().unwrap();
		if let Some(err) = closed.as_ref() {
			session.abort(err.clone());
			return;
		}
		// Before the state publishes, so a watcher that sees this connect already
		// reads its estimate, and the grants that divide it.
		let _ = self.send_bw.set(session.send_bandwidth().and_then(|bw| bw.peek()));
		if let Ok(mut state) = self.state.write() {
			// A migration replaces a live session without a disconnect in between, so
			// its end is counted here, keeping `started - ended` at 1 while connected.
			if state.session.is_some() {
				state.presence.sessions_ended += 1;
			}
			state.revision += 1;
			state.status = Some(Status::Connected);
			state.connects += 1;
			state.last_connected = Some(kio::clock::now());
			state.presence.sessions_started += 1;
			state.failure = None;
			state.version = Some(session.version());
			state.transport = Some(transport);
			state.session = Some(session.clone());
		}
	}

	/// The session in [`State`] keeps serving while its replacement comes up, so
	/// only the status moves.
	fn migrating(&self) {
		self.status(Status::Migrating);
	}

	fn status(&self, status: Status) {
		if let Ok(mut state) = self.state.write() {
			state.revision += 1;
			state.status = Some(status);
		}
	}

	/// Mirror the policy's draining predecessor, for [`Connection::close`].
	fn predecessor(&self, predecessor: Option<(&moq_net::Session, kio::clock::Instant)>) {
		// A write wakes every watcher, so skip one that changes nothing.
		if predecessor.is_none() && self.state.read().predecessor.is_none() {
			return;
		}
		if let Ok(mut state) = self.state.write() {
			state.predecessor = predecessor.map(|(session, deadline)| (session.clone(), deadline));
		}
	}

	/// Nothing is live. Clears the session so a handle can't be handed a closed one,
	/// and drops the estimates that belonged to it.
	fn offline(&self, failure: Option<&Error>) {
		if let Ok(mut state) = self.state.write() {
			state.offline();
			if let Some(failure) = failure {
				state.failure = Some(failure.clone());
			}
		}
		let _ = self.send_bw.set(None);
		let _ = self.recv_bw.set(None);
	}

	/// Record why the loop is retrying, for [`Monitor::failure`].
	fn retrying(&self, err: &Error) {
		if let Ok(mut state) = self.state.write() {
			state.revision += 1;
			state.failure = Some(err.clone());
		}
	}

	/// The loop is over: drop the session and publish the terminal snapshot in one
	/// revision, so a watcher never sees a `Disconnected` the loop will not redial
	/// from. `err` is what it gave up with, for [`Connection::closed`]. Nothing is
	/// retrying anymore, so [`Monitor::failure`] stops reporting.
	fn end(&self, err: Option<Error>) {
		// No session will present the tokens again, so they end with the loop.
		let aborted = self.closed.lock().unwrap().clone();
		let auth_err = match (aborted, &err) {
			(Some(err), _) => err,
			(None, Some(Error::MoqNet(err))) => err.clone(),
			(None, Some(err)) if err.is_auth() => moq_net::Error::Unauthorized,
			(None, Some(err)) => moq_net::Error::Transport(err.to_string()),
			(None, None) => moq_net::Error::Closed,
		};
		if let Ok(mut state) = self.state.write() {
			if state.ended {
				return;
			}
			state.offline();
			// Including when the task was aborted mid-handover: nothing enforces the
			// predecessor's deadline anymore, so a parked clone would hold its transport open.
			// A graceful close took its own clone before aborting.
			state.predecessor = None;
			state.ended = true;
			state.failure = None;
			state.error = err;
		}
		let _ = self.send_bw.set(None);
		let _ = self.recv_bw.set(None);
		self.auth.abort(match auth_err {
			// A token reads `Cancel` as its peer ending the grant cleanly.
			moq_net::Error::Cancel => moq_net::Error::Closed,
			err => err,
		});
	}
}

impl Drop for Shared {
	fn drop(&mut self) {
		// The loop is over, including when its task was aborted out from under it.
		// Whoever still holds a consumer keeps the last state readable, so a
		// [`Monitor`] outliving every [`Connection`] would otherwise
		// keep the session clone parked here alive and the transport open with
		// nothing left able to reach it. Releasing it drops the last clone, which
		// is what closes the transport.
		self.end(None);
	}
}

/// A cloneable observer of a [`Connection`] across reconnects.
///
/// Obtained via [`Connection::monitor`]. [`stats`](Self::stats) returns `None` while the loop is
/// between connections (reconnecting), and `Some` snapshot while a session is established.
#[derive(Clone)]
pub struct Monitor {
	state: kio::Consumer<State>,
	last_presence: moq_net::stats::Presence,
}

/// The current lifecycle phase of a reconnecting connection.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
	/// No session has connected yet.
	Connecting,
	/// A session is serving.
	Connected,
	/// No session is serving while the connection redials.
	Disconnected,
	/// The old session serves while its replacement dials.
	Migrating,
	/// The reconnect loop has stopped for good.
	Closed,
}

/// One coherent observation of a connection's lifecycle.
#[derive(Clone, Debug)]
pub struct Observation {
	/// An opaque revision to pass back to the next state wait.
	pub revision: u64,
	/// The current lifecycle phase, distinct from cumulative statistics presence.
	pub phase: Phase,
	/// How many sessions have connected, including replacements.
	pub connects: u64,
	/// When a session last connected, retained while offline.
	pub last_connected: Option<kio::clock::Instant>,
	/// The live session's transport, or none while offline.
	pub transport: Option<crate::Transport>,
	/// The latest failed attempt or terminal failure, cleared by a successful connect.
	pub failure: Option<Error>,
	/// The live session's negotiated version, or none while offline.
	pub version: Option<Version>,
}

impl State {
	/// Nothing is live: one lifecycle revision that drops the session and its version.
	fn offline(&mut self) {
		// Count one close per live session; a second call without an intervening
		// connect (e.g. Drop after the loop already reported the close) must not
		// move the counter again.
		if self.session.is_some() {
			self.presence.sessions_ended += 1;
		}
		self.revision += 1;
		self.status = Some(Status::Disconnected);
		self.version = None;
		self.transport = None;
		self.session = None;
	}

	fn observation(&self) -> Observation {
		Observation {
			revision: self.revision,
			phase: if self.ended {
				Phase::Closed
			} else {
				match self.status {
					None => Phase::Connecting,
					Some(Status::Connected) => Phase::Connected,
					Some(Status::Disconnected) => Phase::Disconnected,
					Some(Status::Migrating) => Phase::Migrating,
				}
			},
			connects: self.connects,
			last_connected: self.last_connected,
			transport: self.transport,
			failure: self.error.clone().or_else(|| self.failure.clone()),
			version: self.version,
		}
	}
}

impl Monitor {
	/// Read the lifecycle fields together under one lock.
	pub fn state(&self) -> Observation {
		self.state.read().observation()
	}

	/// Poll for a lifecycle revision different from the caller's last snapshot.
	fn poll_state(&self, waiter: &kio::Waiter, after: u64) -> Poll<crate::Result<Observation>> {
		match self.state.poll(waiter, |state| {
			if state.revision != after {
				Poll::Ready(Ok(state.observation()))
			} else if state.ended {
				Poll::Ready(Err(terminal(state)))
			} else {
				Poll::Pending
			}
		}) {
			Poll::Ready(Ok(result)) => Poll::Ready(result),
			Poll::Ready(Err(state)) => Poll::Ready(Err(terminal(&state))),
			Poll::Pending => Poll::Pending,
		}
	}

	/// Wait for a lifecycle revision without sharing a cursor with other observers.
	pub async fn state_changed(&self, after: u64) -> crate::Result<Observation> {
		kio::wait(|waiter| self.poll_state(waiter, after)).await
	}
	/// Cumulative connects and disconnects of this reconnect loop, the same shape as a relay's
	/// sessions track: `sessions_started - sessions_ended` is 1 while connected, and a rate is a delta over
	/// any window.
	pub fn presence(&self) -> moq_net::stats::Presence {
		self.state.read().presence
	}

	/// Poll until either presence counter moves past what this handle last reported.
	pub fn poll_presence(&mut self, waiter: &kio::Waiter) -> Poll<crate::Result<moq_net::stats::Presence>> {
		let last = self.last_presence;
		let presence = match ready!(self.state.poll(waiter, |state| match state.presence {
			presence if presence != last => Poll::Ready(presence),
			_ => Poll::Pending,
		})) {
			Ok(presence) => presence,
			Err(state) => return Poll::Ready(Err(terminal(&state))),
		};

		self.last_presence = presence;
		Poll::Ready(Ok(presence))
	}

	/// Wait until either presence counter moves past what this handle last reported.
	///
	/// Unlike [`Connection::status`], a connect and disconnect that both land before the caller polls
	/// are not coalesced away: the counters still moved.
	pub async fn presence_changed(&mut self) -> crate::Result<moq_net::stats::Presence> {
		kio::wait(|waiter| self.poll_presence(waiter)).await
	}

	/// Snapshot the current connection's stats, or `None` if not currently connected.
	pub fn stats(&self) -> Option<moq_net::session::Stats> {
		self.state.read().session.as_ref().map(moq_net::Session::stats)
	}

	/// Why the loop is retrying: the error that ended the last session or failed the
	/// latest dial, or `None` once a session connects.
	///
	/// Read it while [`Connection::status`] reports [`Status::Disconnected`]: a redial
	/// that keeps failing never changes the status, so this is where its reason shows.
	/// The loop giving up reports through [`Connection::closed`] instead.
	pub fn failure(&self) -> Option<Error> {
		self.state.read().failure.clone()
	}

	/// Snapshot statistics and protocol together, or `None` while disconnected.
	pub fn snapshot(&self) -> Option<Snapshot> {
		let state = self.state.read();
		let session = state.session.as_ref()?;
		Some(Snapshot {
			stats: session.stats(),
			version: session.version(),
		})
	}
}

/// Statistics and protocol sampled from the same live connection.
#[non_exhaustive]
pub struct Snapshot {
	/// Transport statistics at the time of the snapshot.
	pub stats: moq_net::session::Stats,
	/// Protocol negotiated by the connection that supplied these statistics.
	pub version: Version,
}

/// Handle to a connection maintained by a background task.
///
/// The task connects, waits for the session to end, then (unless reconnecting is
/// disabled, see [`crate::Client::with_reconnect`])
/// redials with exponential backoff. The read surface mirrors [`moq_net::Session`]
/// so a caller can treat it like a session that transparently reconnects:
/// [`version`](Self::version), [`send_bandwidth`](Self::send_bandwidth),
/// and [`recv_bandwidth`](Self::recv_bandwidth) track the live session and reset while disconnected.
/// The extra toggle a plain session doesn't have is the connection lifecycle: [`established`](Self::established)
/// waits for the first session, [`connected`](Self::connected) reads the current state synchronously,
/// [`connects`](Self::connects) counts the sessions so far, and [`status`](Self::status) waits for the
/// next change. [`closed`](Self::closed) waits for the loop to stop. Clones share the loop; it
/// stops when the last clone drops (or on an explicit [`abort`](Self::abort)).
#[derive(Clone)]
#[must_use = "dropping the Connection stops the dial; hold it for as long as you want the session"]
pub struct Connection {
	task: std::sync::Arc<Task>,
	state: kio::Consumer<State>,
	/// The auth handle every session of the loop is built on.
	auth: moq_net::auth::Handle,
	/// The tokens every dial of the loop presents.
	tokens: moq_net::auth::Tokens,
	/// Persistent send-bitrate estimate, fed by the loop from each live session.
	send_bandwidth: BandwidthConsumer,
	/// Divides `send_bandwidth`, and every session the loop dials probes for its reservations.
	bandwidth: moq_net::bandwidth::Allocator,
	/// Persistent recv-bitrate estimate, fed by the loop from each live session.
	recv_bandwidth: moq_net::bandwidth::Weak,
	/// Divides `recv_bandwidth`, and every session the loop dials sends its viewers' target.
	viewers: moq_net::bandwidth::Viewers,
	/// The last status returned by [`status`](Self::status), for change detection.
	/// Per-clone: a clone starts from its parent's cursor and diverges from there.
	last_reported: Option<Status>,
}

/// The connection loop, shared by every [`Connection`] clone and aborted when the
/// last one drops.
struct Task {
	handle: AbortOnDrop,
	/// A graceful close retains the loop's resources while its sessions drain.
	running: std::sync::Weak<Running>,
	closed: CloseGuard,
}

/// Resources owned by the loop, retained by a graceful close until its sessions drain.
struct Running {
	shared: Shared,
}

/// Serializes [`Connection::abort`] and [`Connection::close`] against the loop publishing a fresh session.
///
/// Aborting a tokio task doesn't interrupt it before its next yield, so a redial
/// completing in that window would otherwise hand [`Shared::connected`] a session
/// that `abort` had already looked for and missed, and it would be closed by the
/// refcount drop instead of carrying the caller's error code to the peer. Both
/// sides take this lock around their `state` access, so the session is either
/// published before the close is recorded or refused after it.
type CloseGuard = std::sync::Arc<std::sync::Mutex<Option<moq_net::Error>>>;

impl Connection {
	pub(crate) fn new(mut client: Client, addrs: Addrs) -> Self {
		let producer = kio::Producer::<State>::default();
		let state = producer.consume();
		let tokens = moq_net::auth::Tokens::new(client.tokens.clone());
		let auth = tokens.handle();
		client.moq = client.moq.with_auth(auth.clone());

		// The loop feeds these across every reconnect, so a consumer's handle survives session churn
		// (unlike a session's own bandwidth consumer, which dies with the session).
		let send_bw = BandwidthProducer::new();
		let recv_bw = BandwidthProducer::new();
		let send_bandwidth = send_bw.consume();
		let recv_bandwidth = recv_bw.weak();
		let bandwidth = moq_net::bandwidth::Allocator::new(send_bandwidth.clone());
		let viewers = moq_net::bandwidth::Viewers::new(recv_bandwidth.clone());
		let client = client.with_allocator(bandwidth.clone()).with_viewers(viewers.clone());

		let closed: CloseGuard = Default::default();
		let task_closed = closed.clone();
		// Own final-state publication before spawning: aborting an unpolled task
		// must still drop Shared and publish its terminal revision.
		let shared = Shared {
			state: producer,
			send_bw,
			recv_bw,
			closed: task_closed,
			auth: auth.clone(),
			tokens: tokens.clone(),
		};

		let running = std::sync::Arc::new(Running { shared });
		let task_running = std::sync::Arc::downgrade(&running);
		let task = crate::runtime::spawn(async move {
			let shared = &running.shared;
			let reconnect = !client.policy.once;
			// Isolated inside `shared`, so a panic still records a terminal error
			// instead of closing the channels as if the caller had stopped it.
			let res = crate::panic::isolate(Self::run(shared, client, addrs))
				.await
				.unwrap_or(Err(Error::Panic));
			if let Err(err) = res {
				// In one-shot mode the session ending is the expected lifecycle, and
				// its close reason arrives here; don't dress it up as a loop failure.
				match reconnect {
					true => tracing::error!(%err, "connection loop exited"),
					false => tracing::info!(%err, "connection closed"),
				}
				shared.end(Some(err));
			}
			// A graceful close may retain these producers until its sessions finish draining.
		});
		Self {
			task: std::sync::Arc::new(Task {
				handle: AbortOnDrop::new(task),
				running: task_running,
				closed,
			}),
			state,
			auth,
			tokens,
			send_bandwidth,
			recv_bandwidth,
			bandwidth,
			viewers,
			last_reported: None,
		}
	}

	/// Stop the loop now, for every clone, closing the live session with `err`.
	///
	/// `err` is the code the peer sees. Locally this is still a deliberate stop, so
	/// [`closed`](Self::closed) reports `Ok` rather than `err`.
	///
	/// Aborting the task is not enough on its own: it drops the loop's producer, but
	/// the final state stays readable through every surviving handle, and the session
	/// clone parked in it would hold the transport open until the last one went away.
	/// Closing the session here is what makes "stop now" mean it, and closing it with
	/// `err` is what carries the code to the peer, since the drop path only ever
	/// sends a bare `Cancel`.
	pub fn abort(&self, err: moq_net::Error) {
		// Record the close and take the session under one lock, so a redial landing
		// in the abort window is refused rather than parked: see [`CloseGuard`].
		let session = {
			let mut closed = self.task.closed.lock().unwrap();
			*closed = Some(err.clone());
			self.state.read().session.clone()
		};
		if let Some(session) = session {
			session.abort(err);
		}
		self.task.handle.abort();
	}

	/// Stop the loop for every clone, closing the live session (and any predecessor
	/// still finishing after a GOAWAY) after draining its queued data.
	/// Tokens stay presented until both sessions finish draining.
	///
	/// See [`moq_net::Session::close`]: lite-05 and newer wait for the subscriber to
	/// receive finished tracks' tails; older Lite versions and IETF wait only for
	/// transport ACKs. Announcements are withdrawn first, bounded by a one second deadline
	/// (or a predecessor's remaining handover window, if sooner). Call this before
	/// [`Client::close`], which closes the transport without waiting. Returns `Ok`
	/// when nothing was live, and a session's error if it did not drain.
	pub async fn close(self) -> crate::Result<()> {
		// Abort stops redials, but the sessions still need their token grants until
		// the drain completes. Dropping this owner also ends auth if close is cancelled.
		let running = self.task.running.upgrade();
		// Refuse redials and take the sessions under one lock: see [`CloseGuard`].
		let (session, predecessor) = {
			let mut closed = self.task.closed.lock().unwrap();
			*closed = Some(moq_net::Error::Cancel);
			let state = self.state.read();
			(state.session.clone(), state.predecessor.clone())
		};
		self.task.handle.abort();
		let session = async move {
			match session {
				Some(session) => session.close().await,
				None => Ok(()),
			}
		};
		let predecessor = async move {
			let Some((session, deadline)) = predecessor else {
				return Ok(());
			};
			let abort = session.clone();
			let mut close = std::pin::pin!(session.close());
			tokio::select! {
				res = &mut close => res,
				_ = kio::clock::sleep_until(deadline) => {
					abort.abort(moq_net::Error::GoawayTimeout);
					close.await
				}
			}
		};
		let (session, predecessor) = crate::runtime::scope(async { tokio::join!(session, predecessor) }).await;
		drop(running);
		Ok(session.and(predecessor)?)
	}

	/// Drive moq-net's policy: dial each attempt it asks for and publish what it does.
	async fn run(shared: &Shared, client: Client, addrs: Addrs) -> crate::Result<()> {
		#[cfg(feature = "quic")]
		let mut client = client;
		let mut policy = moq_net::Reconnect::new(client.policy, addrs.as_slice().to_vec(), kio::clock::now())
			.map_err(|_| Error::BackoffHealthy)?
			.with_tokens(shared.tokens.clone(), client.versions.clone());
		// The dial in flight, and the address it was asked for. It owns its client, so a
		// fresh port can replace the loop's between walks.
		let mut dial: Option<(url::Url, std::pin::Pin<Box<Dial>>)> = None;
		// Mirrors the live session's estimates while it serves.
		let mut forward: Option<Forward> = None;
		let mut timer: Option<kio::clock::Sleep> = None;
		// Consecutive walks that failed since the last session, and whether a dial failed
		// since then, which tells a failed walk from a session that ended too soon.
		#[cfg(feature = "quic")]
		let (mut failed_walks, mut dial_failed) = (0, false);

		loop {
			let action = kio::wait(|waiter| {
				loop {
					if let Some((_, attempt)) = dial.as_mut()
						&& let Poll::Ready(res) = waiter.poll_future(attempt.as_mut())
					{
						let (url, _) = dial.take().expect("polled above");
						match res {
							Ok(crate::client::Dialed { session, transport }) => {
								tracing::info!(peer = %Endpoint(&url), %transport, "connected");
								#[cfg(feature = "quic")]
								{
									(failed_walks, dial_failed) = (0, false);
								}
								shared.connected(&session, transport);
								forward = Some(Forward::new(&session));
								policy.connected(kio::clock::now(), session);
							}
							Err(err) => {
								#[cfg(feature = "quic")]
								{
									dial_failed = true;
								}
								// An explicit shutdown cannot recover by retrying or opening another socket.
								#[cfg(feature = "quic")]
								let verdict = match quic_scheme(&url) && client.quic_closed() {
									true => Verdict::Fatal,
									false => verdict(&err),
								};
								#[cfg(not(feature = "quic"))]
								let verdict = verdict(&err);
								policy.failed(kio::clock::now(), err, verdict);
							}
						}
					}
					if let (Some(forward), Some(session)) = (forward.as_mut(), policy.session()) {
						forward.poll(shared, session, waiter);
					}

					let at = match policy.poll(kio::clock::now(), waiter) {
						Action::Wait(Some(at)) => at,
						Action::Wait(None) => return Poll::Pending,
						action => return Poll::Ready(action),
					};
					let sleep = timer.get_or_insert_with(|| kio::clock::sleep_until(at));
					if sleep.deadline() != at {
						sleep.reset(at);
					}
					if waiter.poll_future(std::pin::Pin::new(sleep)).is_pending() {
						return Poll::Pending;
					}
				}
			})
			.await;

			match action {
				Action::Dial(attempt) => {
					let url = attempt.target().url().clone();
					let client = client.clone();
					dial = Some((url, Box::pin(async move { client.dial(attempt).await })));
				}
				// The policy already counted the timeout; dropping the dial cancels it.
				Action::Abandon => {
					#[cfg(feature = "quic")]
					{
						dial_failed = true;
					}
					dial = None;
				}
				Action::Migrating => {
					forward = None;
					// After the predecessor is published, so a close woken by this status
					// finds it and honors its handover deadline.
					shared.predecessor(policy.predecessor());
					shared.migrating();
				}
				Action::Retired => shared.predecessor(None),
				Action::Offline(failure) => {
					forward = None;
					shared.offline(failure.as_ref());
				}
				Action::Retrying(err) => {
					shared.retrying(&err);
					// A failed walk of a QUIC peer: retry one transient failure on the same
					// socket, then try another flow before the next walk. Only a retry window
					// that allows a third walk gets there: a black-holed handshake outlasts the
					// default window after the first walk.
					#[cfg(feature = "quic")]
					if std::mem::take(&mut dial_failed) && policy.targets().iter().any(|addr| quic_scheme(addr.url())) {
						failed_walks += 1;
						if failed_walks >= 2 {
							match client.fresh_port() {
								Ok(()) => failed_walks = 0,
								// The old flow may have recovered, so dial it anyway. The count
								// stays at the threshold, so the next failed walk retries the
								// allocation.
								Err(err) => tracing::warn!(%err, "no fresh UDP port; redialing from the old one"),
							}
						}
					}
				}
				// The terminal snapshot does the offline bookkeeping, so a watcher never sees a
				// `Disconnected` the loop will not redial from.
				Action::Done(res) => {
					shared.predecessor(None);
					return res;
				}
				Action::Wait(_) => unreachable!("handled while polling"),
			}
		}
	}

	/// Poll until a session is established (the first connect, or the current one).
	///
	/// `Ready(Ok(session))` while a session is live (during a GOAWAY handover this is the
	/// old session, which still serves), `Ready(Err)` once the loop has stopped, `Pending`
	/// otherwise.
	pub fn poll_established(&self, waiter: &kio::Waiter) -> Poll<crate::Result<()>> {
		match ready!(self.state.poll(waiter, |state| match (state.status, &state.session) {
			(Some(Status::Connected | Status::Migrating), Some(_)) => Poll::Ready(()),
			_ => Poll::Pending,
		})) {
			Ok(()) => Poll::Ready(Ok(())),
			Err(state) => Poll::Ready(Err(terminal(&state))),
		}
	}

	/// Wait until a session is established, handing the connection back.
	///
	/// Returns as soon as a session is live, so the first call waits out the initial dial
	/// (surfacing its error if the loop gives up, e.g. on an auth failure or in one-shot
	/// mode). Useful when a caller wants dial errors up front rather than through
	/// [`closed`](Self::closed).
	///
	/// It consumes and returns the connection so the reconnecting chain,
	/// `client.connect(url).established().await?`, keeps it: the loop lives in this
	/// handle, so a version that handed back the underlying session instead would leave
	/// the caller holding a live transport that silently never redials.
	pub async fn established(self) -> crate::Result<Self> {
		kio::wait(|waiter| self.poll_established(waiter)).await?;
		Ok(self)
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
	/// Returns the current [`Status`]. The loop moves between `Connected`,
	/// `Disconnected`, and `Migrating` (a GOAWAY handover, where the old session is
	/// still serving), so successive calls report changes rather than a fixed
	/// alternation; a status that flips and flips back before the caller polls is
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

	/// How many sessions have been live on this handle: 1 after the first connect,
	/// one more per reconnect (and per GOAWAY migration, which is a new session too).
	///
	/// Advanced in the same write that publishes [`Status::Connected`], so reading
	/// after a `Connected` status never returns a stale count. Zero only if no
	/// session has connected yet, which [`established`](Self::established) rules out.
	pub fn connects(&self) -> u64 {
		self.state.read().connects
	}

	/// The negotiated MoQ version of the live session, or `None` while disconnected.
	///
	/// The [`moq_net::Session::version`] counterpart; `Option` because a reconnecting handle can be
	/// between sessions.
	pub fn version(&self) -> Option<Version> {
		self.state.read().version
	}

	/// What the live session runs on, or `None` while disconnected.
	///
	/// Fixed for a session's lifetime; a reconnect may land on a different one.
	pub fn transport(&self) -> Option<crate::Transport> {
		self.state.read().transport
	}

	/// Observe a GOAWAY from the peer, or `None` while between sessions.
	///
	/// The reconnect loop reads the same GOAWAY to drive migration, reported as
	/// [`Status::Migrating`], and a GOAWAY is a latch rather than a queue, so watching
	/// it here doesn't take it from the loop. Reach for this when you need the message
	/// itself: its redirect URI and deadline, or (in one-shot mode, where the loop
	/// ignores GOAWAY entirely) the fact that one arrived at all.
	///
	/// Scoped to the current session, since that's what a GOAWAY is about; a redial
	/// starts a fresh one.
	pub fn draining(&self) -> Option<moq_net::goaway::Consumer> {
		self.state.read().session.as_ref().map(moq_net::Session::draining)
	}

	/// A consumer for the live session's estimated send bitrate, mirroring
	/// [`moq_net::Session::send_bandwidth`].
	///
	/// Unlike the session's, this handle is persistent: the reconnect loop forwards each session's
	/// estimate into it, so it survives reconnects. Its value is `None` while disconnected or when the
	/// backend has no estimate, and is already set when a watcher sees the connect.
	pub fn send_bandwidth(&self) -> BandwidthConsumer {
		self.send_bandwidth.clone()
	}

	/// Divides [`send_bandwidth`](Self::send_bandwidth) among this connection's senders,
	/// mirroring [`moq_net::Session::allocator`].
	///
	/// Persistent like the estimate: every session the loop dials probes for these
	/// reservations, so they survive reconnects. Reservations report `None` while
	/// disconnected, telling a sender to hold its rate.
	pub fn allocator(&self) -> moq_net::bandwidth::Allocator {
		self.bandwidth.clone()
	}

	/// A consumer for the live session's estimated receive bitrate, mirroring
	/// [`moq_net::Session::recv_bandwidth`]. Persistent across reconnects like
	/// [`send_bandwidth`](Self::send_bandwidth); `None` while disconnected or unavailable.
	pub fn recv_bandwidth(&self) -> BandwidthConsumer {
		self.recv_bandwidth.consume()
	}

	/// Divides [`recv_bandwidth`](Self::recv_bandwidth) among the broadcasts watched over
	/// this connection, mirroring [`moq_net::Session::viewers`].
	///
	/// Persistent like the estimate: every session the loop dials sends these viewers'
	/// target, so they survive reconnects. Viewers report `None` while disconnected.
	pub fn viewers(&self) -> moq_net::bandwidth::Viewers {
		self.viewers.clone()
	}

	/// Poll whether the connection loop has stopped.
	///
	/// `Ready(Err)` if it permanently gave up (reconnect timeout exceeded, an auth
	/// rejection, or the session ending in one-shot mode), `Ready(Ok(()))` if stopped
	/// by dropping the handle, `Pending` while it's still running.
	pub fn poll_closed(&self, waiter: &kio::Waiter) -> Poll<crate::Result<()>> {
		ready!(self.state.poll_closed(waiter));
		Poll::Ready(match &self.state.read().error {
			Some(err) => Err(err.clone()),
			None => Ok(()),
		})
	}

	/// Wait until the connection loop stops.
	pub async fn closed(&self) -> crate::Result<()> {
		kio::wait(|waiter| self.poll_closed(waiter)).await
	}

	/// The auth handle every session of this connection is built on: its tokens are
	/// presented on each reconnect, so a route naming one keeps it, and its grant follows
	/// the live session. It ends with the connection: the abort's or [`Error::MoqNet`]'s
	/// error, [`moq_net::Error::Unauthorized`] for a rejection, [`moq_net::Error::Transport`]
	/// for any other failure, or [`moq_net::Error::Closed`] for a stop. Never
	/// [`moq_net::Error::Cancel`], which a token reads as its peer ending the grant cleanly.
	/// Aborting it ends the connection at its next reconnect, with the abort's error.
	pub fn auth(&self) -> moq_net::auth::Handle {
		self.auth.clone()
	}

	/// The tokens every dial of this connection presents, seeded from
	/// [`connect.tokens`](crate::connect::Config::tokens). Set new ones before the old
	/// ones expire, and read the peer's answer from them.
	pub fn tokens(&self) -> moq_net::auth::Tokens {
		self.tokens.clone()
	}

	/// Observe the connection's statistics and presence across reconnects.
	///
	/// The handle keeps working across reconnects without keeping the connection loop alive.
	pub fn monitor(&self) -> Monitor {
		Monitor {
			state: self.state.clone(),
			last_presence: moq_net::stats::Presence::default(),
		}
	}
}

/// A dial in flight.
type Dial = dyn Future<Output = crate::Result<crate::client::Dialed>> + Send;

/// Whether a dial to `url` runs over the QUIC endpoint, whose socket a fresh port replaces.
#[cfg(feature = "quic")]
fn quic_scheme(url: &url::Url) -> bool {
	matches!(url.scheme(), "http" | "https" | "moqt" | "moql")
}

/// Mirrors one live session's send and receive estimates into the persistent producers, so
/// [`Connection`] consumers track the live estimates across the connection.
struct Forward {
	send: Option<BandwidthConsumer>,
	recv: RecvBandwidth,
}

impl Forward {
	fn new(session: &moq_net::Session) -> Self {
		Self {
			send: session.send_bandwidth(),
			recv: RecvBandwidth::default(),
		}
	}

	/// A `poll_*` step: on return, `waiter` is registered for the next change.
	fn poll(&mut self, shared: &Shared, session: &moq_net::Session, waiter: &kio::Waiter) {
		poll_forward(&mut self.send, &shared.send_bw, waiter);
		self.recv.poll(&shared.recv_bw, waiter, || session.recv_bandwidth());
	}
}

/// Own the live session's receive consumer only while an external consumer needs it.
#[derive(Default)]
struct RecvBandwidth {
	// Watches `out`, taken on the first poll.
	demand: Option<moq_net::bandwidth::Demand>,
	wanted: bool,
	source: Option<BandwidthConsumer>,
}

impl RecvBandwidth {
	fn poll(
		&mut self,
		out: &BandwidthProducer,
		waiter: &kio::Waiter,
		source: impl FnOnce() -> Option<BandwidthConsumer>,
	) {
		let demand = self.demand.get_or_insert_with(|| out.demand());
		// Drain every edge, so `waiter` is armed for the next one. Demand can arrive during
		// retirement, which the next turn of the loop reports.
		loop {
			match demand.poll_wanted_changed(waiter) {
				Poll::Pending => break,
				Poll::Ready(Err(_)) => {
					self.source = None;
					return;
				}
				Poll::Ready(Ok(wanted)) => {
					self.wanted = wanted;
					if !wanted {
						self.source = None;
						let _ = out.set(None);
					}
				}
			}
		}
		if !self.wanted {
			return;
		}
		if self.source.is_none() {
			self.source = source();
		}
		poll_forward(&mut self.source, out, waiter);
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

/// The terminal error read from a closed channel's final state.
///
/// No recorded error means the loop was stopped locally rather than giving up, so
/// this reports [`Error::Stopped`]: [`Connection::closed`] treats that as the `Ok`
/// it is, and a watcher that only has an error to go on can still tell it apart
/// from a connection that failed.
fn terminal(state: &State) -> Error {
	match &state.error {
		Some(err) => err.clone(),
		None => Error::Stopped,
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use url::Url;

	/// A millisecond track: the frames written here carry timestamps.
	fn timed() -> moq_net::track::Info {
		moq_net::track::Info::default().with_timescale(moq_net::Timescale::MILLI)
	}

	/// Updating with an empty CLI preserves a standing TOML value over typed defaults.
	#[test]
	fn cli_does_not_clobber_toml_backoff() {
		#[derive(usage_rs::Cli)]
		#[usage(unknown_flags = "error", args_override_self = false)]
		#[usage(settings)]
		struct Wrapper {
			#[usage(flatten)]
			backoff: Backoff,
		}

		// Stand in for the TOML layer, then re-apply the CLI with none of the flags set.
		let mut parsed = Wrapper::parse_from(&[]).unwrap();
		parsed.backoff.initial = Some((Duration::from_secs(7)).into());
		parsed.backoff.multiplier = 5;
		parsed.backoff.max = Some((Duration::from_secs(11)).into());
		parsed.backoff.healthy = Some((Duration::from_secs(2)).into());
		parsed.backoff.timeout = Some((Duration::ZERO).into());
		parsed.update_from(&[]);

		assert_eq!(parsed.backoff.initial, Some((Duration::from_secs(7)).into()));
		assert_eq!(parsed.backoff.multiplier, 5);
		assert_eq!(parsed.backoff.max, Some((Duration::from_secs(11)).into()));
		assert_eq!(parsed.backoff.healthy, Some((Duration::from_secs(2)).into()));
		assert_eq!(parsed.backoff.timeout(), Duration::ZERO, "0 means retry forever");

		// With no flags, the parser defaults resolve to the policy's defaults.
		let parsed = Wrapper::parse_from(&[]).unwrap();
		assert_eq!(parsed.backoff.resolve(), moq_net::reconnect::Backoff::default());

		// And a flag still lands where the merge can see it.
		let parsed = Wrapper::parse_from(&[
			std::ffi::OsStr::new("--connect-backoff-initial"),
			std::ffi::OsStr::new("3s"),
		])
		.unwrap();
		assert_eq!(parsed.backoff.resolve().initial, Duration::from_secs(3));
	}

	/// GOAWAY typed defaults remain overrideable by a standing TOML layer.
	#[test]
	fn cli_does_not_clobber_toml_goaway() {
		#[derive(usage_rs::Cli)]
		#[usage(unknown_flags = "error", args_override_self = false)]
		#[usage(settings)]
		struct Wrapper {
			#[usage(flatten)]
			goaway: Goaway,
		}

		// No flags passed: the typed defaults are present.
		let parsed = Wrapper::parse_from(&[]).unwrap();
		assert_eq!(parsed.goaway.redirect, Redirect::SameHost);
		assert_eq!(parsed.goaway.resolve(), moq_net::reconnect::Goaway::default());

		// Flags passed: they land where the merge can see them.
		let parsed = Wrapper::parse_from(&[
			std::ffi::OsStr::new("--connect-goaway-redirect"),
			std::ffi::OsStr::new("ignore"),
			std::ffi::OsStr::new("--connect-goaway-handover"),
			std::ffi::OsStr::new("3s"),
		])
		.unwrap();
		assert_eq!(parsed.goaway.redirect, Redirect::Ignore);
		let resolved = parsed.goaway.resolve();
		assert_eq!(resolved.redirect, moq_net::reconnect::Redirect::Ignore);
		assert_eq!(resolved.handover, Duration::from_secs(3));
	}

	/// A zero healthy bar would count every session healthy, so it is refused at build.
	#[test]
	fn a_zero_healthy_bar_is_refused() {
		let mut config = crate::connect::Config::default();
		config.backoff.healthy = Some((Duration::ZERO).into());
		let err = config
			.init(Default::default())
			.err()
			.expect("zero healthy built a client");
		assert!(matches!(err, Error::BackoffHealthy), "{err}");
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
		src.set(Some(moq_net::bandwidth::Rate::from_bps(3_000))).unwrap();
		poll_forward(&mut bw, &out, &waiter);
		assert_eq!(out_rx.peek(), Some(moq_net::bandwidth::Rate::from_bps(3_000)));

		// The estimate becoming unavailable is mirrored, but the arm stays: the
		// backend reporting nothing right now is not the session ending.
		src.set(None).unwrap();
		poll_forward(&mut bw, &out, &waiter);
		assert_eq!(out_rx.peek(), None);
		assert!(bw.is_some());

		// So a later value on the same live session still gets through. Dropping the
		// arm on the `None` above would have stranded the estimate at `None` for the
		// rest of the session.
		src.set(Some(moq_net::bandwidth::Rate::from_bps(9_000))).unwrap();
		poll_forward(&mut bw, &out, &waiter);
		assert_eq!(out_rx.peek(), Some(moq_net::bandwidth::Rate::from_bps(9_000)));

		// Closing the source is what retires the arm, so we stop polling a dead one.
		drop(src);
		poll_forward(&mut bw, &out, &waiter);
		assert!(bw.is_none());
	}

	#[test]
	fn receive_demand_is_forwarded_across_sessions_without_becoming_demand() {
		let out = BandwidthProducer::new();
		let handle = out.weak();
		let waiter = kio::Waiter::noop();
		let first = BandwidthProducer::new();
		first.set(Some(moq_net::bandwidth::Rate::from_bps(123))).unwrap();
		let mut forward = RecvBandwidth::default();
		forward.poll(&out, &waiter, || Some(first.consume()));
		assert!(!first.demand().wanted(), "the connection handle is not demand");
		let reader = handle.consume();
		forward.poll(&out, &waiter, || Some(first.consume()));
		assert!(first.demand().wanted());
		assert_eq!(reader.peek(), Some(moq_net::bandwidth::Rate::from_bps(123)));
		let clone = reader.clone();
		drop(reader);
		forward.poll(&out, &waiter, || Some(first.consume()));
		assert!(first.demand().wanted(), "a remaining clone is demand");
		drop(clone);
		forward.poll(&out, &waiter, || Some(first.consume()));
		assert!(
			!first.demand().wanted(),
			"the last drop releases the live session's consumer"
		);
		let reader = handle.consume();
		assert_eq!(reader.peek(), None, "an unused estimate is cleared");
		forward.poll(&out, &waiter, || Some(first.consume()));
		assert!(first.demand().wanted(), "demand can resume on the same session");
		drop(forward);
		assert!(!first.demand().wanted());
		out.set(None).unwrap();
		assert_eq!(reader.peek(), None, "disconnected");
		let second = BandwidthProducer::new();
		second.set(Some(moq_net::bandwidth::Rate::from_bps(456))).unwrap();
		let mut forward = RecvBandwidth::default();
		forward.poll(&out, &waiter, || Some(second.consume()));
		assert!(
			second.demand().wanted(),
			"the same reader demands the replacement session"
		);
		assert_eq!(reader.peek(), Some(moq_net::bandwidth::Rate::from_bps(456)));
		drop(reader);
		forward.poll(&out, &waiter, || Some(second.consume()));
		assert!(!second.demand().wanted());
	}

	#[test]
	fn receive_demand_arriving_during_retirement_is_forwarded() {
		use std::sync::{Arc, Mutex};
		use std::task::{Wake, Waker};

		struct Demand {
			handle: moq_net::bandwidth::Weak,
			consumer: Mutex<Option<BandwidthConsumer>>,
		}
		impl Wake for Demand {
			fn wake(self: Arc<Self>) {
				self.wake_by_ref();
			}
			fn wake_by_ref(self: &Arc<Self>) {
				*self.consumer.lock().unwrap() = Some(self.handle.consume());
			}
		}

		let out = BandwidthProducer::new();
		out.set(Some(moq_net::bandwidth::Rate::from_bps(123))).unwrap();
		let demand = Arc::new(Demand {
			handle: out.weak(),
			consumer: Mutex::new(None),
		});
		let waiter = kio::Waiter::new(Waker::from(demand.clone()));
		let mut reader = out.consume();
		assert!(reader.poll_changed(&waiter).is_ready());
		assert!(reader.poll_changed(&waiter).is_pending());
		drop(reader);
		let source = BandwidthProducer::new();
		let mut forward = RecvBandwidth::default();
		// Clearing the unused estimate wakes the old reader's waiter. It creates
		// fresh demand precisely between the unused and used checks.
		forward.poll(&out, &kio::Waiter::noop(), || Some(source.consume()));
		assert!(demand.consumer.lock().unwrap().is_some());
		assert!(
			source.demand().wanted(),
			"new demand must be forwarded without unrelated activity"
		);
	}

	const WALK_TIMEOUT: Duration = Duration::from_secs(30);

	/// A retry delay and healthy bar short enough to stay out of a test's way.
	const FAST: Duration = Duration::from_millis(50);

	fn url(value: &str) -> Url {
		value.parse().expect("valid url")
	}

	/// A loopback TCP port that hangs up on every connection, so dialing it fails fast.
	///
	/// A dead address that fails fast rather than black-holes is what keeps these
	/// tests fast and deterministic: the walk itself is what's under test, and
	/// bounding a black-holed attempt is covered by
	/// [`only_a_candidate_with_a_fallback_is_bounded`] as a pure function.
	///
	/// It listens rather than leaving the port merely bound: only Linux refuses a
	/// SYN to a socket that is bound but not listening, while macOS drops it and
	/// each attempt waits out its timeout. Listening also keeps the port reserved,
	/// so no other listener (or an ephemeral self-connect) can take it mid-test.
	#[cfg(feature = "tcp")]
	async fn dead() -> Url {
		let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind");
		let addr = listener.local_addr().expect("local addr");
		// Dropping each stream unread hangs up before the qmux handshake completes.
		tokio::spawn(async move { while listener.accept().await.is_ok() {} });
		url(&format!("tcp://{addr}/"))
	}

	/// A bound stream listener and the URL that reaches it.
	#[cfg(feature = "tcp")]
	async fn live() -> (crate::Listener, Url) {
		let mut config = crate::listen::Config::default();
		config.tcp.bind = Some("127.0.0.1:0".parse().unwrap());
		let listener = config
			.init(Default::default())
			.expect("build server")
			.listen()
			.await
			.expect("listen");
		let addr = listener.tcp_local_addr().expect("tcp listener bound");
		(listener, url(&format!("tcp://{addr}/")))
	}

	/// A client that trusts the self-signed [`live`] listener.
	#[cfg(feature = "tcp")]
	fn client() -> Client {
		let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();

		let mut config = crate::connect::Config::default();
		config.tls.insecure = Some(true);
		config.init(Default::default()).expect("build client")
	}

	#[test]
	fn observation_keeps_independent_revisions_and_never_misses_registration() {
		use std::sync::{
			Arc,
			atomic::{AtomicUsize, Ordering},
		};
		use std::task::{Wake, Waker};
		struct Wakes(AtomicUsize);
		impl Wake for Wakes {
			fn wake(self: Arc<Self>) {
				self.wake_by_ref();
			}
			fn wake_by_ref(self: &Arc<Self>) {
				self.0.fetch_add(1, Ordering::SeqCst);
			}
		}
		let shared = shared();
		let first = Monitor {
			state: shared.state.consume(),
			last_presence: Default::default(),
		};
		let second = first.clone();
		let initial = first.state();
		assert_eq!(initial.phase, Phase::Connecting);
		assert_eq!(initial.connects, 0);
		assert!(initial.version.is_none());
		let failure = Error::ConnectFailed;
		// The update precedes registration: reading a token must not lose this wake.
		shared.retrying(&failure);
		let waiter = kio::Waiter::noop();
		let Poll::Ready(Ok(next)) = first.poll_state(&waiter, initial.revision) else {
			panic!("missed earlier update")
		};
		assert_eq!(next.phase, Phase::Connecting);
		assert!(next.failure.is_some());
		assert!(
			matches!(second.poll_state(&waiter, initial.revision), Poll::Ready(Ok(_))),
			"independent observer lost its update"
		);
		// Register first: even another failure with the same diagnostic must wake it.
		let wakes = Arc::new(Wakes(AtomicUsize::new(0)));
		let registered = kio::Waiter::new(Waker::from(wakes.clone()));
		assert!(first.poll_state(&registered, next.revision).is_pending());
		shared.retrying(&failure);
		assert!(wakes.0.load(Ordering::SeqCst) > 0);
		let Poll::Ready(Ok(repeated)) = first.poll_state(&waiter, next.revision) else {
			panic!("identical failed attempt was hidden")
		};
		assert_ne!(repeated.revision, next.revision);
		shared.end(Some(failure));
		drop(shared);
		let final_state = first.state();
		assert_eq!(final_state.phase, Phase::Closed);
		assert!(final_state.failure.is_some(), "terminal cause must remain observable");
		assert!(
			matches!(first.poll_state(&waiter, repeated.revision), Poll::Ready(Ok(state)) if state.phase == Phase::Closed)
		);
		assert!(matches!(
			first.poll_state(&waiter, final_state.revision),
			Poll::Ready(Err(_))
		));
	}

	#[tokio::test]
	async fn cancelled_observation_does_not_advance_another_waiter() {
		let shared = shared();
		let monitor = Monitor {
			state: shared.state.consume(),
			last_presence: Default::default(),
		};
		let revision = monitor.state().revision;
		{
			let mut abandoned = std::pin::pin!(monitor.state_changed(revision));
			assert!(futures::poll!(&mut abandoned).is_pending());
		}
		shared.retrying(&Error::ConnectFailed);
		let first = monitor.state_changed(revision).await.unwrap();
		let second = monitor.state_changed(revision).await.unwrap();
		assert_eq!(first.revision, second.revision);
	}

	/// Records the phase a watcher reads the moment it is woken, as one on another thread could.
	struct Woken(Monitor, std::sync::Mutex<Vec<Phase>>);

	impl std::task::Wake for Woken {
		fn wake(self: std::sync::Arc<Self>) {
			self.wake_by_ref();
		}
		fn wake_by_ref(self: &std::sync::Arc<Self>) {
			self.1.lock().unwrap().push(self.0.state().phase);
		}
	}

	impl Woken {
		/// Park on the monitor's current revision, for as long as the returned waiter lives.
		fn watch(monitor: &Monitor) -> (std::sync::Arc<Self>, kio::Waiter) {
			let woken = std::sync::Arc::new(Self(monitor.clone(), Default::default()));
			let waiter = kio::Waiter::new(std::task::Waker::from(woken.clone()));
			assert!(monitor.poll_state(&waiter, monitor.state().revision).is_pending());
			(woken, waiter)
		}

		fn phases(&self) -> Vec<Phase> {
			self.1.lock().unwrap().clone()
		}
	}

	#[test]
	fn ending_the_loop_never_shows_a_redialing_phase() {
		let shared = shared();
		let monitor = Monitor {
			state: shared.state.consume(),
			last_presence: Default::default(),
		};
		let (woken, _waiter) = Woken::watch(&monitor);
		drop(shared);
		assert_eq!(woken.phases(), [Phase::Closed]);
	}

	#[cfg(feature = "tcp")]
	#[tokio::test]
	async fn aborting_before_the_loop_is_polled_publishes_its_final_snapshot() {
		// The current-thread runtime cannot poll the spawned loop before our first await.
		let connection = Connection::new(
			client(),
			Addrs::collect([Url::parse("http://127.0.0.1:1").unwrap()]).unwrap(),
		);
		let monitor = connection.monitor();
		let initial = monitor.state();
		connection.abort(moq_net::Error::Cancel);
		connection.closed().await.unwrap();
		let final_state = monitor.state_changed(initial.revision).await.unwrap();
		assert_eq!(final_state.phase, Phase::Closed);
		assert_eq!(final_state.connects, 0);
		assert!(monitor.state_changed(final_state.revision).await.is_err());
	}

	fn shared() -> Shared {
		let tokens = moq_net::auth::Tokens::new(Vec::new());
		Shared {
			state: kio::Producer::default(),
			send_bw: BandwidthProducer::new(),
			recv_bw: BandwidthProducer::new(),
			closed: CloseGuard::default(),
			auth: tokens.handle(),
			tokens,
		}
	}

	/// The first address that answers wins, however many dead ones precede it.
	/// This is what keeps a peer reachable when discovery advertises an interface
	/// that doesn't route from here ahead of the one that does.
	#[cfg(feature = "tcp")]
	#[tokio::test]
	async fn the_walk_lands_past_the_dead_addresses() {
		let (mut server, live) = live().await;
		let (accepted, mut sessions) = tokio::sync::mpsc::unbounded_channel();
		tokio::spawn(async move {
			while let Ok(request) = server.accept().await {
				let _ = accepted.send(());
				let _ = request.ok().await;
			}
		});

		let dead_a = dead().await;
		let dead_b = dead().await;
		let addrs = Addrs::collect([dead_a, dead_b, live.clone()]).expect("not empty");
		let connection = kio::clock::timeout(
			WALK_TIMEOUT,
			client().with_reconnect(false).connect(addrs).established(),
		)
		.await
		.expect("the walk must not hang")
		.expect("the live address must connect");
		assert_eq!(connection.connects(), 1);
		assert!(
			sessions.recv().await.is_some(),
			"the walk must land on the one that answers"
		);
	}

	/// With nothing reachable the walk reports a failure rather than hanging.
	#[cfg(feature = "tcp")]
	#[tokio::test]
	async fn the_walk_fails_when_nothing_answers() {
		let dead_a = dead().await;
		let dead_b = dead().await;
		let addrs = Addrs::collect([dead_a, dead_b]).expect("not empty");
		let connection = client().with_reconnect(false).connect(addrs);
		let result = kio::clock::timeout(WALK_TIMEOUT, connection.closed())
			.await
			.expect("the walk must not hang");
		assert!(result.is_err(), "every address hung up, so the walk must fail");
	}

	/// Dialing must not write the LAN mesh's membership credential to the log.
	///
	/// The proof rides as a URL path segment, since raw QUIC has no headers to put
	/// it in, and a dial URL logged verbatim would hand a replayable credential to
	/// anyone who can read logs. This drives the real `tracing` stack rather than
	/// [`Endpoint`]'s `Display`, so it also catches a log site that forgot to use
	/// it. moq-net's own log sites have a test of their own.
	#[cfg(feature = "tcp")]
	#[tracing_test::traced_test]
	#[tokio::test]
	async fn dialing_never_logs_the_credential() {
		const SECRET: &str = "b91d7fe20c4a";

		let mut target = dead().await;
		target.set_path(&format!("/.cluster/{SECRET}"));

		let err = client()
			.with_reconnect(false)
			.connect(target)
			.closed()
			.await
			.expect_err("a dead address must fail");

		// The log line did happen, so the assertion below isn't vacuously true.
		assert!(logs_contain("connection closed"), "the loop never logged at all");
		assert!(!logs_contain(SECRET), "a log line leaked the credential");
		assert!(!format!("{err}").contains(SECRET), "the error leaked it: {err}");
	}

	/// Invalid local configuration must end an unlimited reconnect loop without waiting.
	#[cfg(all(feature = "quic", feature = "websocket"))]
	#[tokio::test(start_paused = true)]
	async fn an_unsupported_scheme_ends_unlimited_reconnects() {
		let mut config = crate::connect::Config::default();
		config.tls.insecure = Some(true);
		config.backoff.timeout = Some((Duration::ZERO).into());
		let client = config.init(Default::default()).expect("build client");
		let started = kio::clock::now();
		let connection = client.connect(url("moqz://localhost/"));
		let err = kio::clock::timeout(Duration::from_secs(1), connection.closed())
			.await
			.expect("invalid scheme kept retrying")
			.expect_err("invalid scheme connected");
		assert!(err.is_terminal(), "{err}");
		assert_eq!(started.elapsed(), Duration::ZERO);
	}

	/// A URL host that parses but can never be a TLS server name ends an unlimited reconnect loop.
	#[cfg(all(feature = "quic", feature = "websocket"))]
	#[tokio::test(start_paused = true)]
	async fn an_invalid_tls_server_name_ends_unlimited_reconnects() {
		let mut config = crate::connect::Config::default();
		config.tls.insecure = Some(true);
		config.backoff.timeout = Some((Duration::ZERO).into());
		let client = config.init(Default::default()).expect("build client");
		for target in [
			"https://a!b:1/",
			"wss://a!b:1/",
			"moqt://a!b:1/",
			#[cfg(feature = "tcp")]
			"tls://a!b:1/",
		] {
			let connection = client.connect(url(target));
			let err = kio::clock::timeout(Duration::from_secs(1), connection.closed())
				.await
				.unwrap_or_else(|_| panic!("{target} kept retrying"))
				.expect_err("invalid server name connected");
			assert!(err.is_terminal(), "{target}: {err}");
		}
	}

	/// A refused socket still spends the retry budget rather than stopping on its first error.
	#[cfg(feature = "tcp")]
	#[tokio::test(start_paused = true)]
	async fn a_refused_tcp_dial_keeps_retrying() {
		// Refusal is what's under test, so this can't use [`dead`]. Only Linux
		// refuses a SYN to a port that is bound but not listening, so only Linux
		// holds it: a released port can become another process's listener mid-test.
		// Elsewhere the port is released, since only a free port refuses there.
		let reserve = socket2::Socket::new(socket2::Domain::IPV4, socket2::Type::STREAM, None).expect("socket");
		let loopback = std::net::SocketAddr::from((std::net::Ipv4Addr::LOCALHOST, 0));
		reserve.bind(&loopback.into()).expect("bind");
		let addr = reserve
			.local_addr()
			.expect("local addr")
			.as_socket()
			.expect("an inet addr");
		#[cfg(not(target_os = "linux"))]
		drop(reserve);
		// The OS's own wording, which differs on Windows.
		let refused = std::net::TcpStream::connect(addr)
			.expect_err("nothing listens")
			.to_string();

		let mut config = crate::connect::Config::default();
		config.backoff.initial = Some((Duration::from_millis(50)).into());
		config.backoff.max = Some((Duration::from_millis(50)).into());
		config.backoff.timeout = Some((Duration::from_millis(100)).into());
		// No timer may be pending while a dial is in flight, since the paused clock
		// jumps to it whenever the runtime idles. Linux and macOS refuse a loopback
		// connect inside the call, but Windows reports it later, after retrying the
		// SYN, so the connect deadline or the Happy Eyeballs stagger would fire first.
		config.timeout = Some((Duration::ZERO).into());
		config.race = Some((Duration::ZERO).into());
		let client = config.init(Default::default()).expect("build client");

		let started = kio::clock::now();
		let connection = client.connect(url(&format!("tcp://{addr}/")));
		let err = connection.closed().await.expect_err("refused socket connected");
		assert!(matches!(err, Error::Reconnect(_)), "{err}");
		assert!(err.to_string().contains(&refused), "{err}");
		assert!(started.elapsed() >= Duration::from_millis(100));
	}

	/// A redial that keeps failing reports why while it retries, and the connect that
	/// finally lands clears it. Without this a caller only learns the reason once the
	/// loop gives up, which with no retry window is never.
	#[cfg(feature = "tcp")]
	#[tokio::test]
	async fn a_failing_redial_reports_why_until_it_connects() {
		// Hold the failing listener until the runtime ends: a released port can
		// become another process's listener while the connection keeps retrying.
		let failing = dead().await.socket_addrs(|| None).unwrap();
		assert!(
			std::net::TcpListener::bind(failing[0]).is_err(),
			"the failing port stays reserved"
		);
		let (live, mut accepted, _task) = serve(crate::origin::spawn()).await;

		// Unique to this test: the table is process-wide.
		const HOST: &str = "failing-redial.test";
		crate::resolve::hosts::point(HOST, failing);

		let mut config = crate::connect::Config::default();
		config.backoff.initial = Some((FAST).into());
		config.backoff.max = Some((FAST).into());
		config.backoff.timeout = Some((Duration::ZERO).into());
		let client = config.init(Default::default()).unwrap();
		let mut connection = client.connect(url(&format!("tcp://{HOST}:1/")));
		let monitor = connection.monitor();

		let failure = kio::wait(|waiter| {
			monitor.state.poll(waiter, |state| match &state.failure {
				Some(err) => Poll::Ready(err.clone()),
				None => Poll::Pending,
			})
		})
		.await
		.unwrap_or_else(|_| panic!("the connection ended before reporting its retry reason"));
		assert_eq!(
			monitor
				.failure()
				.expect("the monitor reports the retry reason")
				.to_string(),
			failure.to_string()
		);
		assert!(
			matches!(&failure, Error::Tcp(err) if matches!(err.as_ref(), crate::tcp::Error::Connect(reason) if !reason.is_empty())),
			"{failure}"
		);
		let transition = futures::FutureExt::now_or_never(connection.status());
		assert!(transition.is_none(), "a failed redial is no status transition");

		crate::resolve::hosts::point(HOST, [live]);
		assert_eq!(connection.status().await.unwrap(), Status::Connected);
		assert!(monitor.failure().is_none(), "the connect must clear the failure");
		let _session = accepted.recv().await.unwrap();
	}

	/// Closing a connection between retries ends the retry, so its reason goes with it.
	#[cfg(feature = "tcp")]
	#[tokio::test]
	async fn closing_between_retries_clears_the_failure() {
		// A held port, not a released one: another test's server could claim a
		// released port, and the dial would connect instead of failing.
		let mut config = crate::connect::Config::default();
		config.backoff.initial = Some((FAST).into());
		config.backoff.max = Some((FAST).into());
		config.backoff.timeout = Some((Duration::ZERO).into());
		let client = config.init(Default::default()).unwrap();
		let connection = client.connect(dead().await);
		let mut monitor = connection.monitor();

		kio::wait(|waiter| {
			monitor.state.poll(waiter, |state| match state.failure {
				Some(_) => Poll::Ready(()),
				None => Poll::Pending,
			})
		})
		.await
		.unwrap_or_else(|_| panic!("the connection ended before reporting its retry reason"));

		connection.close().await.expect("close");
		// Resolves with an error once the loop's state is final.
		let ended = monitor.presence_changed().await;
		assert!(ended.is_err(), "nothing connected, so only the end moves presence");
		assert!(monitor.failure().is_none(), "nothing is retrying");
	}

	/// The connection's auth handle presents its token on every session the loop makes,
	/// without the application adding it again.
	#[cfg(feature = "tcp")]
	#[tokio::test]
	async fn a_token_is_presented_on_every_session() {
		const WAIT: Duration = Duration::from_secs(10);
		let version: moq_net::Version = "moq-lite-07-wip".parse().unwrap();

		let mut config = crate::listen::Config::default();
		config.tcp.bind = Some("127.0.0.1:0".parse().unwrap());
		config.version = vec![version];
		let mut listener = config.init(Default::default()).unwrap().listen().await.unwrap();
		let addr = listener.tcp_local_addr().unwrap();

		let mut config = crate::connect::Config {
			version: vec![version],
			..Default::default()
		};
		config.backoff.initial = Some((FAST).into());
		config.backoff.max = Some((FAST).into());
		let connection = config
			.init(Default::default())
			.unwrap()
			.connect(url(&format!("tcp://{addr}/")));
		let auth = connection.auth();
		let added = tokio::spawn({
			let auth = auth.clone();
			async move { auth.add("token").await }
		});

		// Held past each session's abort: dropping a grant closes its stream cleanly, which
		// is the peer's verdict ending the token.
		let mut issued = Vec::new();
		let mut added = Some(added);
		let mut token = None;
		for round in 0..2 {
			let mut request = kio::clock::timeout(WAIT, listener.accept()).await.unwrap().unwrap();
			let mut requests = request.requests();
			let session = request.ok().await.unwrap();
			loop {
				let request = kio::clock::timeout(WAIT, requests.next()).await.unwrap().unwrap();
				if request.token().is_empty() {
					issued.push(request.accept(moq_net::auth::Grant::default()));
					continue;
				}
				assert_eq!(request.token().as_ref(), b"token", "round {round}");
				issued.push(request.accept(moq_net::auth::Grant::all()));
				break;
			}
			if let Some(added) = added.take() {
				token = Some(added.await.unwrap().expect("granted").0);
			}
			let mut grant = token.as_ref().unwrap().grant();
			while grant.peek() != Some(moq_net::auth::Grant::all()) {
				kio::clock::timeout(WAIT, grant.changed()).await.unwrap().unwrap();
			}
			session.abort(moq_net::Error::Cancel);
		}
		drop(connection);
		let token = token.unwrap();
		// Not `Cancel`, which reads as the peer ending the grant cleanly.
		let closed = kio::clock::timeout(WAIT, token.closed()).await.unwrap();
		assert!(matches!(closed, moq_net::Error::Closed), "{closed:?}");
	}

	/// Ending the connection's auth handle ends the loop at its next reconnect, rather than
	/// redialing sessions the handle refuses.
	#[cfg(feature = "tcp")]
	#[tokio::test]
	async fn an_ended_auth_handle_ends_the_loop() {
		const WAIT: Duration = Duration::from_secs(10);

		let (addr, mut accepted, _task) = serve(crate::origin::spawn()).await;
		let mut config = crate::connect::Config::default();
		config.backoff.initial = Some((FAST).into());
		config.backoff.max = Some((FAST).into());
		let connection = config
			.init(Default::default())
			.unwrap()
			.connect(url(&format!("tcp://{addr}/")));

		let session = kio::clock::timeout(WAIT, accepted.recv()).await.unwrap().unwrap();
		connection.auth().abort(moq_net::Error::App(9));
		session.abort(moq_net::Error::Cancel);
		let closed = kio::clock::timeout(WAIT, connection.closed())
			.await
			.expect("the loop kept redialing on an ended auth handle");
		assert!(
			matches!(closed, Err(Error::MoqNet(moq_net::Error::App(9)))),
			"{closed:?}"
		);
	}

	/// A session that ends the loop reports through `closed`, not as a retry reason.
	#[cfg(feature = "tcp")]
	#[tokio::test]
	async fn a_session_that_ends_the_loop_reports_no_failure() {
		const WAIT: Duration = Duration::from_secs(10);

		let (addr, mut accepted, _task) = serve(crate::origin::spawn()).await;
		let client = crate::connect::Config::default()
			.init(Default::default())
			.unwrap()
			.with_reconnect(false);
		let connection = client.connect(url(&format!("tcp://{addr}/")));
		let monitor = connection.monitor();

		let session = kio::clock::timeout(WAIT, accepted.recv()).await.unwrap().unwrap();
		let mut state = monitor.state();
		while state.phase != Phase::Connected {
			state = kio::clock::timeout(WAIT, monitor.state_changed(state.revision))
				.await
				.unwrap()
				.unwrap();
		}
		let (woken, _waiter) = Woken::watch(&monitor);
		session.abort(moq_net::Error::App(7));
		let closed = kio::clock::timeout(WAIT, connection.closed())
			.await
			.expect("the connection did not close");
		assert!(closed.is_err(), "the abort ends the loop with an error");
		assert!(monitor.failure().is_none(), "nothing is retrying");
		assert_eq!(woken.phases(), [Phase::Closed], "the loop is not redialing");
	}

	/// A settled answer from one candidate ends the walk instead of being buried.
	///
	/// The walk keeps only the last failure, so without this a `404` from the first
	/// address would be overwritten by a transport error from the second, and the
	/// outer loop would retry the whole list until its budget ran out over a
	/// question the peer had already answered. Only the statuses that invite
	/// another attempt keep the walk going.
	#[test]
	fn a_settled_status_stops_the_walk() {
		let settled = [401, 403, 404, 400, 500];
		for status in settled {
			assert!(
				!crate::error::status_retryable(status),
				"{status} is the peer's answer, so the walk should stop"
			);
		}

		// The ones worth asking again about: request timeout, rate limit, and the
		// gateway/overload statuses. A different address may well do better.
		for status in [408, 429, 502, 503, 504] {
			assert!(
				crate::error::status_retryable(status),
				"{status} invites another attempt, so the walk should continue"
			);
		}
	}

	/// An auth rejection or malformed handshake settles the walk like a status, so a transient
	/// failure from another candidate cannot replace it and keep an unlimited loop retrying.
	#[test]
	fn an_auth_rejection_stops_the_walk() {
		assert_eq!(
			verdict(&Error::Connect(crate::ConnectError::Unauthorized)),
			Verdict::Refused
		);
		assert_eq!(
			verdict(&Error::Connect(crate::ConnectError::Forbidden)),
			Verdict::Refused
		);
		assert_eq!(verdict(&Error::MoqNet(moq_net::Error::Unauthorized)), Verdict::Refused);
		assert_eq!(
			verdict(&Error::MoqNet(moq_net::Error::ProtocolViolation)),
			Verdict::Fatal
		);
		assert_eq!(verdict(&Error::MoqNet(moq_net::Error::Closed)), Verdict::Retry);
		assert_eq!(
			verdict(&Error::Io(std::sync::Arc::new(
				std::io::ErrorKind::ConnectionRefused.into()
			))),
			Verdict::Retry
		);
		assert_eq!(verdict(&Error::NoBackend("none")), Verdict::Unusable);
	}

	/// Relays QUIC datagrams to `server`, holding them all until [`Forwarder::open`].
	///
	/// Holding is what decides the transport race without depending on timing: the
	/// QUIC dial cannot finish until the test says so.
	#[cfg(all(feature = "websocket", feature = "quic"))]
	struct Forwarder {
		gate: tokio::sync::watch::Sender<bool>,
	}

	#[cfg(all(feature = "websocket", feature = "quic"))]
	impl Forwarder {
		async fn start(front: tokio::net::UdpSocket, server: std::net::SocketAddr) -> Self {
			use std::sync::Arc;

			let back = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
			back.connect(server).await.unwrap();
			let (front, back) = (Arc::new(front), Arc::new(back));
			let (tx, rx) = tokio::sync::watch::channel(false);
			let (client_tx, client_rx) = tokio::sync::watch::channel(None);

			// Each datagram waits for the gate on its own task.
			fn relay(gate: &tokio::sync::watch::Receiver<bool>, send: impl Future<Output = ()> + Send + 'static) {
				let mut gate = gate.clone();
				tokio::spawn(async move {
					if gate.wait_for(|open| *open).await.is_ok() {
						send.await;
					}
				});
			}

			{
				let (front, back, gate) = (front.clone(), back.clone(), rx.clone());
				tokio::spawn(async move {
					let mut buf = vec![0; 65536];
					while let Ok((len, from)) = front.recv_from(&mut buf).await {
						client_tx.send_replace(Some(from));
						let packet = buf[..len].to_vec();
						let back = back.clone();
						relay(&gate, async move {
							let _ = back.send(&packet).await;
						});
					}
				});
			}

			tokio::spawn(async move {
				let mut buf = vec![0; 65536];
				while let Ok(len) = back.recv(&mut buf).await {
					let Some(client) = *client_rx.borrow() else { continue };
					let packet = buf[..len].to_vec();
					let front = front.clone();
					relay(&rx, async move {
						let _ = front.send_to(&packet, client).await;
					});
				}
			});

			Self { gate: tx }
		}

		/// Start forwarding, releasing whatever was held.
		fn open(&self) {
			self.gate.send_replace(true);
		}
	}

	/// When QUIC wins the race the WebSocket dial is cancelled at once: its socket
	/// closes without carrying a session, and the one session runs over QUIC.
	#[cfg(all(feature = "websocket", feature = "quic"))]
	#[tokio::test]
	async fn quic_winning_cancels_the_websocket_dial() {
		use tokio::io::AsyncReadExt;
		const WAIT: Duration = Duration::from_secs(10);

		let mut listen = crate::listen::Config {
			bind: Some("127.0.0.1:0".parse().unwrap()),
			..Default::default()
		};
		listen.tls.generate = vec!["localhost".into()];
		let config = crate::server::Config {
			listen,
			..Default::default()
		};
		let mut server = config.init().unwrap().listen().await.unwrap();
		let server_addr = server.local_addr().unwrap();
		let (accepted, mut sessions) = tokio::sync::mpsc::unbounded_channel();
		tokio::spawn(async move {
			while let Ok(request) = server.accept().await {
				let transport = request.transport();
				if let Ok(session) = request.ok().await {
					let _ = accepted.send((transport, session));
				}
			}
		});

		// The fallback dials the URL's port over TCP, and QUIC the same number over UDP,
		// which the forwarder takes. Nothing reserves the pair, so retry on a collision.
		let (fallback, front) = 'bind: {
			for _ in 0..20 {
				let fallback = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
				let port = fallback.local_addr().unwrap().port();
				if let Ok(front) = tokio::net::UdpSocket::bind(("127.0.0.1", port)).await {
					break 'bind (fallback, front);
				}
			}
			panic!("could not bind a matching TCP and UDP port after 20 attempts");
		};
		let port = front.local_addr().unwrap().port();
		let forwarder = Forwarder::start(front, server_addr).await;

		// Take the fallback's socket but never answer its upgrade, and only then let QUIC
		// through: QUIC wins with both dials under way. The socket closing is the client
		// cancelling the dial that lost.
		let cancelled = tokio::spawn(async move {
			let (mut socket, _) = fallback.accept().await.unwrap();
			forwarder.open();
			let mut buf = [0; 1024];
			while matches!(socket.read(&mut buf).await, Ok(len) if len > 0) {}
		});

		let mut config = crate::connect::Config::default();
		config.tls.insecure = Some(true);
		config.websocket.delay = Some((Duration::ZERO).into());
		let client = config.init(Default::default()).unwrap();
		let url = url(&format!("http://127.0.0.1:{port}/"));
		let connection = kio::clock::timeout(WAIT, client.connect(url).established())
			.await
			.expect("never connected")
			.unwrap();
		assert_eq!(connection.transport(), Some(crate::Transport::WebTransport));
		let (transport, _session) = kio::clock::timeout(WAIT, sessions.recv())
			.await
			.expect("the server never accepted a session")
			.unwrap();
		assert_eq!(transport, crate::Transport::WebTransport);

		kio::clock::timeout(WAIT, cancelled)
			.await
			.expect("the WebSocket dial outlived QUIC's win")
			.unwrap();
		assert_eq!(connection.connects(), 1);
	}

	/// A stream-only server on a free loopback port, publishing `origin`.
	///
	/// Returns its address, a receiver yielding each accepted session (so the test
	/// can drain it), and the listener task.
	#[cfg(feature = "tcp")]
	async fn serve(
		origin: moq_net::origin::Producer,
	) -> (
		std::net::SocketAddr,
		tokio::sync::mpsc::UnboundedReceiver<moq_net::Session>,
		tokio::task::JoinHandle<()>,
	) {
		let mut config = crate::listen::Config::default();
		config.tcp.bind = Some("127.0.0.1:0".parse().unwrap());
		let mut server = config.init(Default::default()).unwrap().listen().await.unwrap();
		let addr = server.tcp_local_addr().expect("tcp listener bound");

		let (accepted, sessions) = tokio::sync::mpsc::unbounded_channel();
		let task = tokio::spawn(async move {
			while let Ok(request) = server.accept().await {
				if let Ok(session) = request.with_publisher(&origin).ok().await {
					let _ = accepted.send(session);
				}
			}
		});
		(addr, sessions, task)
	}

	/// The fleet drain: a relay withdrawn from DNS sends an empty-URI GOAWAY with a
	/// deadline, and the client lands on a healthy relay by resolving the configured
	/// name again. The drained relay still accepts, so a cached resolve would land
	/// right back on it. The live track hands over at a group boundary, and the old
	/// session closes at our handover cap, well before the peer's own deadline.
	#[cfg(feature = "tcp")]
	#[tokio::test]
	async fn a_fleet_drain_redials_through_a_fresh_resolve() {
		const WAIT: Duration = Duration::from_secs(10);
		const HANDOVER: Duration = Duration::from_millis(500);
		const DEADLINE: Duration = Duration::from_secs(30);

		// Two relays of one fleet, serving the same live broadcast.
		let origin = crate::origin::spawn();
		let broadcast = origin.create_broadcast("cam").unwrap();
		broadcast.announce(Default::default()).unwrap();
		let track = broadcast.create_track("video", timed()).unwrap();
		let (addr_a, mut accepted_a, _task_a) = serve(origin.clone()).await;
		let (addr_b, mut accepted_b, _task_b) = serve(origin.clone()).await;

		// Unique to this test: the table is process-wide.
		const HOST: &str = "fleet-drain.test";
		crate::resolve::hosts::point(HOST, [addr_a]);

		let subscriber = crate::origin::spawn();
		let mut config = crate::connect::Config::default();
		config.goaway.handover = Some((HANDOVER).into());
		// A session younger than the healthy bar counts as redirected immediately and
		// waits out a backoff, which this test is not about.
		config.backoff.initial = Some((FAST).into());
		config.backoff.healthy = Some((FAST).into());
		let client = config
			.init(Default::default())
			.unwrap()
			.with_subscriber(subscriber.clone());
		let url: Url = format!("tcp://{HOST}:1/").parse().unwrap();
		let _connection = client.connect(url);

		let session_a = kio::clock::timeout(WAIT, accepted_a.recv()).await.unwrap().unwrap();

		let consumer = subscriber.consume();
		let cam = kio::clock::timeout(WAIT, consumer.follow("cam", None).unwrap().available())
			.await
			.unwrap()
			.unwrap();
		let mut sub = cam.track("video").unwrap().subscribe(None).await.unwrap();

		let mut group = track.append_group().unwrap();
		group.write_frame(moq_net::Timestamp::ZERO, b"g0".as_ref()).unwrap();
		group.finish().unwrap();
		let g0 = kio::clock::timeout(WAIT, sub.recv_group())
			.await
			.unwrap()
			.unwrap()
			.unwrap();
		assert_eq!(g0.sequence, 0);

		// Withdraw A from DNS, then drain it.
		crate::resolve::hosts::point(HOST, [addr_b]);
		let drained = kio::clock::now();
		session_a
			.drain()
			.send(moq_net::goaway::Goaway::new().with_timeout(DEADLINE))
			.unwrap();

		let _session_b = kio::clock::timeout(WAIT, accepted_b.recv())
			.await
			.expect("never redialed through the fresh resolve")
			.unwrap();

		let mut group = track.append_group().unwrap();
		group.write_frame(moq_net::Timestamp::ZERO, b"g1".as_ref()).unwrap();
		group.finish().unwrap();
		let mut g1 = kio::clock::timeout(WAIT, sub.recv_group())
			.await
			.unwrap()
			.unwrap()
			.unwrap();
		assert_eq!(g1.sequence, 1, "delivery resumes at the next group after the swap");
		assert_eq!(g1.read_frame().await.unwrap().unwrap().payload[..], b"g1"[..]);

		kio::clock::timeout(WAIT, session_a.closed())
			.await
			.expect("the drained session never closed");
		assert!(
			drained.elapsed() < DEADLINE,
			"the old session outlived the handover cap and waited for the peer's deadline"
		);
		assert!(
			accepted_a.try_recv().is_err(),
			"a cached resolve redialed the drained relay"
		);
	}

	/// Under [`Restart::Wait`] an empty-URI GOAWAY changes nothing while the peer keeps
	/// the session: no migration and no redial. Its close then reconnects like any drop.
	#[cfg(feature = "tcp")]
	#[tokio::test]
	async fn a_waited_restart_redials_only_once_the_session_closes() {
		use futures::FutureExt;
		const WAIT: Duration = Duration::from_secs(10);

		let origin = crate::origin::spawn();
		let (addr, mut accepted, _task) = serve(origin.clone()).await;

		let subscriber = crate::origin::spawn();
		let mut config = crate::connect::Config::default();
		config.backoff.initial = Some((FAST).into());
		config.backoff.healthy = Some((FAST).into());
		let client = config
			.init(Default::default())
			.unwrap()
			.with_subscriber(subscriber.clone())
			.with_restart(Restart::Wait);
		let mut connection = client.connect(url(&format!("tcp://{addr}/")));
		let status = kio::clock::timeout(WAIT, connection.status()).await.unwrap();
		assert_eq!(status.unwrap(), Status::Connected);
		let session = kio::clock::timeout(WAIT, accepted.recv()).await.unwrap().unwrap();

		session.drain().send(moq_net::goaway::Goaway::new()).unwrap();

		// A broadcast announced after the GOAWAY still arrives: the drained session serves.
		let late = origin.create_broadcast("late").unwrap();
		late.announce(Default::default()).unwrap();
		kio::clock::timeout(WAIT, subscriber.consume().follow("late", None).unwrap().available())
			.await
			.expect("the drained session stopped serving")
			.unwrap();
		assert!(
			connection
				.draining()
				.is_some_and(|goaway| matches!(goaway.poll(&kio::Waiter::noop()), Poll::Ready(Ok(_)))),
			"the GOAWAY never arrived"
		);
		assert_eq!(
			connection.status().now_or_never().transpose().unwrap(),
			None,
			"the loop left the session"
		);
		assert!(accepted.try_recv().is_err(), "redialed while the session served");

		session.abort(moq_net::Error::GoawayTimeout);
		let status = kio::clock::timeout(WAIT, connection.status()).await.unwrap();
		assert_eq!(status.unwrap(), Status::Disconnected);
		let status = kio::clock::timeout(WAIT, connection.status()).await.unwrap();
		assert_eq!(status.unwrap(), Status::Connected);
		kio::clock::timeout(WAIT, accepted.recv())
			.await
			.expect("never redialed after the close")
			.unwrap();
	}

	/// A configured token is held before its dial starts the session, so a broadcast
	/// published before the dial, which only that token's grant covers, waits for its
	/// answer instead of closing the session against the connection credential's grant.
	#[cfg(feature = "tcp")]
	#[tokio::test(start_paused = true)]
	async fn a_publication_waits_for_a_configured_token() {
		let version: moq_net::Version = "moq-lite-07-wip".parse().unwrap();
		let publish = crate::origin::spawn();
		let broadcast = publish.create_broadcast("room").unwrap();
		broadcast.announce(moq_net::origin::Route::default()).unwrap();
		let server_origin = crate::origin::spawn();

		let config = crate::connect::Config {
			tokens: vec!["connection".parse().unwrap(), "publish".parse().unwrap()],
			version: vec![version],
			..Default::default()
		};
		let mut client = config.init(Default::default()).expect("build client");
		let server_dial = server_origin.clone();
		client.dialer = Some(std::sync::Arc::new(
			move |_addr: crate::connect::Addr, moq: moq_net::Client| {
				let server_origin = server_dial.clone();
				Box::pin(async move {
					let versions = moq_net::Versions::from(version);
					let config = || {
						let mut config = qmux::Config::new(qmux::Version::QMux01);
						config.protocol =
							qmux::Protocol::Negotiate(versions.alpns().iter().map(|alpn| alpn.to_string()).collect());
						config
					};
					let (client, server) = tokio::io::duplex(64 * 1024);
					let (client, server) = tokio::try_join!(
						qmux::Session::connect(qmux::transport::Stream::new(client), config()),
						qmux::Session::accept(qmux::transport::Stream::new(server), config())
					)
					.unwrap();
					let (client, server) = (crate::spawn_qmux(client), crate::spawn_qmux(server));
					let now = moq_net::time::Instant::now();
					let client = moq.connect(now, client);
					let server = async {
						let mut request = moq_net::Server::new()
							.with_versions(versions.clone())
							.with_subscriber(server_origin)
							.accept_request(now, server)
							.await
							.unwrap();
						// The connection credential grants nothing; only the configured token
						// covers the broadcast.
						let mut requests = request.requests();
						let (session, driver) = request.ok().await.unwrap();
						tokio::spawn(moq_net::time::run(driver));
						tokio::spawn(async move {
							let _session = session;
							let mut issued = Vec::new();
							while let Some(request) = requests.next().await {
								let grant = match request.token().is_empty() {
									true => moq_net::auth::Grant::default(),
									false => moq_net::auth::Grant::all(),
								};
								issued.push(request.accept(grant));
							}
						});
					};
					let client = async {
						let (session, driver) = client.await.unwrap();
						tokio::spawn(moq_net::time::run(driver));
						session
					};
					let (session, ()) = tokio::join!(client, server);
					// Leave the session the time a multithreaded runtime may give it before the dial
					// returns: on the paused clock, every exchange the session can make lands first,
					// the connection credential's answer included.
					kio::clock::sleep(Duration::from_millis(10)).await;
					Ok(crate::client::Dialed {
						session,
						transport: crate::Transport::Tcp,
					})
				})
			},
		));

		let connection = client
			.with_publisher(&publish)
			.connect("moqt://relay.example.com/".parse::<Url>().unwrap())
			.established()
			.await
			.expect("the dial failed");

		let mut announced = server_origin.consume().announced();
		let arrived = kio::clock::timeout(Duration::from_secs(5), announced.next())
			.await
			.expect("the broadcast never reached the server")
			.expect("the server's origin closed");
		assert!(matches!(arrived, moq_net::announce::Event::Start(update) if update.prefix.as_str() == "room"));
		assert!(
			connection.connected() && connection.connects() == 1,
			"the session closed before the configured token was answered"
		);
	}
}

#[cfg(test)]
mod duration_tests {
	use super::*;
	crate::cli::duration::test_config_duration!(
		initial_set_in_code_beats_parser_default,
		Backoff,
		initial,
		"--connect-backoff-initial",
		|c: &Backoff| c.initial(),
		|d| d
	);
	crate::cli::duration::test_config_duration!(
		max_set_in_code_beats_parser_default,
		Backoff,
		max,
		"--connect-backoff-max",
		|c: &Backoff| c.max(),
		|d| d
	);
	crate::cli::duration::test_config_duration!(
		healthy_set_in_code_beats_parser_default,
		Backoff,
		healthy,
		"--connect-backoff-healthy",
		|c: &Backoff| c.resolve().healthy,
		|d| d
	);
	crate::cli::duration::test_config_duration!(
		timeout_set_in_code_beats_parser_default,
		Backoff,
		timeout,
		"--connect-backoff-timeout",
		|c: &Backoff| c.timeout(),
		|d| d
	);
	crate::cli::duration::test_config_duration!(
		handover_set_in_code_beats_parser_default,
		Goaway,
		handover,
		"--connect-goaway-handover",
		|c: &Goaway| c.resolve().handover,
		|d| d
	);
}
