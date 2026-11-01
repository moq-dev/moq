//! The reconnect loop around a session, and the [`Monitor`] that observes it.

use std::task::{Poll, ready};
use std::time::Duration;

use moq_net::Version;
use moq_net::bandwidth::{Consumer as BandwidthConsumer, Producer as BandwidthProducer};
use moq_net::kio;
use rand::RngExt;
use url::Url;

use crate::abort::AbortOnDrop;
use crate::connect::Endpoint;
use crate::{Addrs, Client, Error};

/// How long one address gets before [`Connection`] moves to the peer's next one.
const CONNECT_ATTEMPT: Duration = Duration::from_secs(5);

/// The deadline for the attempt at candidate `index` of `total`, or `None` to
/// let it run.
///
/// A black-holed address answers nothing rather than refusing, so without a
/// bound the QUIC idle timeout would decide how long the remaining candidates
/// wait. Only an attempt with a later candidate to fall back on is worth
/// bounding: cutting the last one short just turns a slow connect into a failed
/// one, on every reconnect cycle.
///
/// The same goes for the retry window from [`Backoff::timeout`]: it decides
/// whether a new walk starts, not how long one may take, so a walk under way
/// tries every candidate and a handshake slower than the window still gets
/// [`crate::connect::Config::timeout`](crate::connect::Config::timeout).
fn attempt_timeout(index: usize, total: usize) -> Option<Duration> {
	(index + 1 < total).then_some(CONNECT_ATTEMPT)
}

/// Whether `err` refused the credential a dial presented while the client has since been
/// given new tokens, which earns another dial instead of ending the loop. It never
/// retries a rejected credential: a refusal of unchanged tokens stays terminal.
fn refreshed(err: &Error, tokens: &moq_net::auth::Tokens, presented: Option<&moq_net::auth::Dial>) -> bool {
	let refused = err.is_auth()
		|| matches!(err, Error::MoqNet(err) if moq_net::SessionError::from(err) == moq_net::SessionError::Unauthorized);
	refused && presented.is_some_and(|presented| tokens.changed(presented))
}

/// Whether a failed candidate is the peer's answer for every candidate, ending the walk.
///
/// An auth rejection arrives as [`Error::Connect`] with its status already consumed, so it
/// is checked alongside the statuses that do not invite another attempt. A terminal MoQ error
/// means the handshake reached the peer, which answered with malformed input.
fn settles_walk(err: &Error) -> bool {
	err.is_auth()
		|| (matches!(err, Error::MoqNet(_)) && err.is_terminal())
		|| err
			.status()
			.is_some_and(|status| !crate::error::status_retryable(status))
}

/// Exponential backoff configuration for reconnection attempts.
///
/// Three knobs, each with one job: `initial` and `multiplier` set the wait before
/// each retry, `max` caps it, and `healthy` decides which sessions count as failed
/// attempts. The delays carry jitter, so a fleet knocked offline together does not
/// reconnect in lockstep. The timeout bounds every failure; a settled rejection,
/// malformed protocol input, or invalid local configuration stops retrying. A GOAWAY
/// naming no new URI is no failure: a restarting peer sends one to each session until
/// it goes, and each of those serves, so it restarts the timeout instead.
///
/// A GOAWAY off a healthy session migrates at once, unless it sends the connection back
/// to an endpoint that already redirected it elsewhere and the session lived less than
/// `max`. Two peers redirecting to each other thus wait out this backoff and count
/// against the timeout.
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
			multiplier: DEFAULT_MULTIPLIER,
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
		self.initial.map(Into::into).unwrap_or(DEFAULT_INITIAL)
	}

	/// The configured retry-delay cap, or its default; zero leaves the delay uncapped.
	pub fn max(&self) -> Duration {
		self.max.map(Into::into).unwrap_or(DEFAULT_MAX)
	}

	/// The retry deadline window, with zero meaning unlimited and unset using the default.
	pub fn timeout(&self) -> Duration {
		self.timeout.map(Into::into).unwrap_or(DEFAULT_TIMEOUT)
	}

	/// How long a session must last to count as healthy.
	fn healthy(&self) -> Duration {
		self.healthy.map(Into::into).unwrap_or(DEFAULT_HEALTHY)
	}

	/// Refuse a zero healthy threshold, which would count every session as healthy and
	/// reset the timeout forever under a peer that drops each one on sight.
	#[cfg(feature = "_transport")]
	pub(crate) fn validate(&self) -> crate::Result<()> {
		if self.healthy().is_zero() {
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

/// What to do with the URI a peer names in its GOAWAY.
///
/// The URI is dialed exactly as given, so it must carry whatever credentials the
/// new endpoint needs. Nothing from the current session is copied onto it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, usage_rs::ValueEnum, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
#[non_exhaustive]
pub enum Redirect {
	/// Follow any URI that does not downgrade the scheme, letting the peer name the
	/// host as well as the port.
	///
	/// The host is dialed as written, so this hands the peer whatever this process
	/// can reach: a name it controls can carry an A record for loopback, a private
	/// range, or the metadata service, and nothing here resolves it to find out. A
	/// *literal* loopback, private, link-local, or IPC target is still refused, but
	/// that catches a misconfigured peer, not a hostile one. Use it only with an
	/// upstream trusted with the local network and with the tokens on
	/// [`Connection::auth`], which every session presents, on any host.
	Follow,
	/// Follow only when the host matches the configured URL, so a peer can move us
	/// between ports or schemes but not to another host. The default.
	#[default]
	SameHost,
	/// Ignore the URI and retry the current address list.
	Ignore,
}

/// What to do when a peer's GOAWAY names no URI, meaning it is restarting where we reached it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub enum Restart {
	/// Redial the same addresses at once while the drained session keeps serving. Right for
	/// an address that can lead to another node, such as a fleet name a fresh resolve moves
	/// off the drained one. The default.
	#[default]
	Redial,
	/// Keep serving on the drained session until the peer closes it, then reconnect as after
	/// any drop. Right for an address that names one node, where a redial only reaches the
	/// draining process again. The peer must close the session itself, as a relay does at
	/// its drain deadline.
	Wait,
}

impl Redirect {
	/// The URL a GOAWAY assigns. `Ok(None)` keeps the current address list (the
	/// peer named no URI, or the policy ignores a URI it could parse), `Ok(Some)`
	/// replaces it, and `Err` is an explicit URI this policy refuses. A malformed
	/// URI is refused even under [`Self::Ignore`].
	///
	/// `pinned` is a certificate pin on the connection, which can only verify the
	/// host it was configured for, so it refuses a host change even under
	/// [`Self::Follow`].
	fn target(&self, uri: &str, current: &Url, pinned: bool) -> crate::Result<Option<Url>> {
		if uri.is_empty() {
			return Ok(None);
		}

		// The URI can carry credentials, so the error names the reason, never the URI.
		// Parse before `Ignore`: a malformed redirect is terminal even when the policy
		// would otherwise stay on the current address list.
		let refuse = |reason: &str| Error::RefusedRedirect(reason.to_string());

		let target = uri.parse::<Url>().map_err(|_| refuse("the GOAWAY URI is malformed"))?;
		if matches!(self, Self::Ignore) {
			return Ok(None);
		}

		if scheme_tier(target.scheme()) < scheme_tier(current.scheme()) {
			return Err(refuse("the GOAWAY redirect downgrades the scheme"));
		}

		// Only as far as the URL itself says: a name is dialed, never resolved here,
		// so this refuses a peer that names a local address outright and says
		// nothing about one that hides the same address behind a hostname. That gap
		// is why [`Self::SameHost`] is the default; see [`is_local`].
		if is_local(&target) && !is_local(current) {
			return Err(refuse("the GOAWAY redirect widens reachability to a local address"));
		}

		// Host only, not the full authority: the port is what a peer legitimately
		// moves us across when it hands off to a sibling process on the same box.
		let same_host = target.host_str() == current.host_str();
		if matches!(self, Self::SameHost) && !same_host {
			return Err(refuse("the GOAWAY redirect leaves the current host"));
		}
		if pinned && !same_host {
			return Err(refuse("the GOAWAY redirect leaves the host a certificate pin verifies"));
		}

		Ok(Some(target))
	}
}

/// Whether this scheme's dial installs the Rustls verifier, so a configured
/// fingerprint actually checked the peer. Plain and non-Rustls transports do not.
fn fingerprint_pins(scheme: &str) -> bool {
	matches!(scheme, "https" | "wss" | "moqt" | "moql" | "tls")
}

/// Rank a scheme so a peer-supplied redirect cannot silently drop encryption.
/// Unknown schemes rank lowest, so a forgotten classification is refused.
fn scheme_tier(scheme: &str) -> u8 {
	match scheme {
		"https" | "moqt" | "moql" | "wss" | "iroh" | "tls" => 2,
		"tcp" | "ws" | "http" => 1,
		// `unix` lands here deliberately: local IPC is not an upgrade over a
		// network transport, it is a different reachability class (see `is_local`).
		_ => 0,
	}
}

/// Whether a URL *says* it names something only reachable from this host or
/// network.
///
/// A judgement about the URL, not about where a dial would land. Literals and
/// `localhost` are decided here; every other name is left alone, since the
/// address behind it is chosen by whoever runs its DNS and can change between
/// this call and the dial. So a `false` here means "not local on its face", never
/// "verified remote".
fn is_local(url: &Url) -> bool {
	match url.host() {
		// A non-special scheme (`moqt://127.0.0.1`, and every other scheme this
		// crate dials but `http`/`https`/`ws`/`wss`) parses its host as a domain
		// even when it is an address literal, so the literal check can't be left to
		// the URL parser. `crate::resolve::Candidates` handles the same thing.
		Some(url::Host::Domain(host)) => match host.parse::<std::net::IpAddr>() {
			Ok(ip) => is_local_ip(ip),
			Err(_) => host == "localhost" || host.ends_with(".localhost"),
		},
		Some(url::Host::Ipv4(ip)) => is_local_ip(ip.into()),
		Some(url::Host::Ipv6(ip)) => is_local_ip(ip.into()),
		// No host at all, e.g. a `unix:` socket path.
		None => true,
	}
}

/// Whether an address literal is one only this host or network can reach.
fn is_local_ip(ip: std::net::IpAddr) -> bool {
	match ip {
		std::net::IpAddr::V4(ip) => is_local_v4(ip),
		// An IPv4-mapped address reaches the same host as the v4 it wraps, so judge
		// it by that rather than by the v6 rules.
		std::net::IpAddr::V6(ip) => match ip.to_ipv4_mapped() {
			Some(v4) => is_local_v4(v4),
			None => ip.is_loopback() || ip.is_unspecified() || ip.is_unique_local() || ip.is_unicast_link_local(),
		},
	}
}

fn is_local_v4(ip: std::net::Ipv4Addr) -> bool {
	ip.is_loopback() || ip.is_private() || ip.is_link_local() || ip.is_unspecified()
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

	/// How long the old session keeps serving after its replacement connects, e.g.
	/// "10s" or "500ms". This is a cap: a GOAWAY naming a shorter deadline wins,
	/// since the peer force-closes at its own deadline regardless, but a longer one
	/// does not extend it. Defaults to 10 seconds.
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

/// Defaults for the [`Backoff`] knobs, applied by its accessors when a field is unset.
const DEFAULT_INITIAL: Duration = Duration::from_secs(1);
const DEFAULT_MULTIPLIER: u32 = 2;
const DEFAULT_MAX: Duration = Duration::from_secs(5);
const DEFAULT_HEALTHY: Duration = Duration::from_secs(1);
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(10);

/// The retry pacing a connection actually runs on, resolved once from [`Backoff`].
#[derive(Clone, Copy)]
struct Pacing {
	/// The first delay, already under the cap.
	initial: Duration,
	/// The delay cap, or `None` for uncapped.
	max: Option<Duration>,
	/// At least 1: a multiplier of zero means no growth, not a delay that shrinks to nothing.
	multiplier: u32,
	/// The bar a session must clear to count as healthy. Never zero.
	healthy: Duration,
}

impl Pacing {
	fn new(backoff: &Backoff) -> Self {
		let max = Some(backoff.max()).filter(|max| !max.is_zero());
		let initial = backoff.initial();
		Self {
			initial: max.map_or(initial, |max| initial.min(max)),
			max,
			multiplier: backoff.multiplier.max(1),
			healthy: backoff.healthy(),
		}
	}

	/// The delay for the attempt after one that waited `delay`.
	fn next(&self, delay: Duration) -> Duration {
		let next = delay.saturating_mul(self.multiplier);
		self.max.map_or(next, |max| next.min(max))
	}

	/// How long a session must last to start a new run of redirects.
	///
	/// One that outlived the capped delay already redials no faster than the backoff would
	/// allow. Uncapped, the healthy bar is the only rate the config names.
	fn settled(&self) -> Duration {
		self.max.unwrap_or(self.healthy)
	}
}

/// Default handover window, and the ceiling a GOAWAY deadline is capped to when
/// the config names none.
const DEFAULT_HANDOVER: Duration = Duration::from_secs(10);

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

	fn resolve(&self) -> Resolved {
		Resolved {
			redirect: self.redirect,
			handover: self.handover.map(Into::into).unwrap_or(DEFAULT_HANDOVER),
		}
	}
}

#[derive(Clone, Copy)]
struct Resolved {
	redirect: Redirect,
	handover: Duration,
}

impl Resolved {
	fn handover(self, timeout: Option<Duration>) -> Duration {
		std::cmp::min(self.handover, timeout.unwrap_or(Duration::MAX))
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
	/// The loop's [`Draining`] predecessor and its handover deadline, so
	/// [`Connection::close`] drains it too without overstaying that window.
	predecessor: Option<(moq_net::Session, moq_clock::Instant)>,
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

	/// Nothing is live. Clears the session so a handle can't be handed a closed one,
	/// and drops the estimates that belonged to it.
	fn disconnected(&self) {
		self.offline(None);
	}

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
		let client = client.with_allocator(bandwidth.clone());

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
			let reconnect = client.reconnect;
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
	/// See [`moq_net::Session::close`]: explicit lite-07-wip waits for the subscriber
	/// to receive finished tracks' tails; older Lite versions and IETF wait only for
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
				_ = moq_clock::sleep_until(deadline) => {
					abort.abort(moq_net::Error::GoawayTimeout);
					close.await
				}
			}
		};
		let (session, predecessor) = crate::runtime::scope(async { tokio::join!(session, predecessor) }).await;
		drop(running);
		Ok(session.and(predecessor)?)
	}

	async fn run(shared: &Shared, client: Client, addrs: Addrs) -> crate::Result<()> {
		let backoff = client.backoff.clone();
		let goaway = client.goaway.resolve();
		let pacing = Pacing::new(&backoff);
		let timeout = backoff.timeout();
		let mut retry = Retry::new(pacing.initial);
		// Sticky across migrations: a redirect is an assignment, not a detour, so a
		// later drop redials wherever we were last sent. Scoped to this loop, so a
		// fresh Connection starts from the configured addresses again.
		let mut addrs = addrs;
		// An old session kept alive after a GOAWAY so its in-flight groups finish.
		let mut draining: Option<Draining> = None;
		// The current run of migrations, to tell a redirect loop from a healthy handover.
		let mut redirects = Redirects::default();

		loop {
			if !timeout.is_zero() && retry.start.elapsed() >= timeout {
				return Err(timeout_error(timeout, retry.last_error.as_ref()));
			}
			// An ended auth handle refuses every later session, so no dial can recover. A fresh
			// watch only reports an error once its handle has ended.
			if let Poll::Ready(Err(err)) = shared.auth.grant().poll_changed(&kio::Waiter::noop()) {
				return Err(err.into());
			}

			// What the last dial presented, so a refusal is judged against it.
			let mut presented = None;
			match Self::dial_any(shared, &client, &addrs, &mut draining, &mut presented).await {
				Ok((addr, dialed)) => {
					let url = addr.url().clone();
					tracing::info!(peer = %Endpoint(&url), transport = %dialed.transport, "connected");
					shared.connected(&dialed.session, dialed.transport);
					shared.tokens.connected(&dialed.session);
					let session = dialed.session;

					let connected = moq_clock::now();
					// Wait for the session to end, forwarding its bandwidth estimates into the
					// persistent producers meanwhile so consumers track the live stats across the
					// connection, and draining any predecessor left over from a migration.
					let ended = run_session(shared, &session, &mut draining, client.restart).await;
					// A refusal is terminal unless the tokens changed since this dial read them.
					let refreshed = match &ended {
						Ended::Closed(Err(err)) => refreshed(err, &shared.tokens, presented.as_ref()),
						_ => false,
					};

					let tenure = connected.elapsed();

					// The connected target owns the policy, including in one-shot mode. A
					// refused redirect is terminal: the peer is leaving and named somewhere we
					// won't go, so redialing the old address or a fallback would ignore it.
					let assigned = match &ended {
						// A fingerprint only checked the dial that installed the Rustls
						// verifier. tcp, unix, iroh, and plaintext WebSocket never consult it.
						Ended::Goaway(msg) => {
							goaway
								.redirect
								.target(msg.uri(), &url, client.pinned && fingerprint_pins(url.scheme()))?
						}
						Ended::Closed(_) => None,
					};
					if assigned.is_some() && addr.addresses().is_some() {
						return Err(Error::PinnedRedirect);
					}

					// One-shot mode leaves rather than migrating: there is no replacement to
					// dial, and a GOAWAY naming no deadline never force-closes, so the peer
					// stops accepting requests and waits for us. Ignoring it is how both
					// sides end up waiting on each other forever. Checked before the
					// migration branch below, which would otherwise loop back to redial.
					if !client.reconnect && matches!(ended, Ended::Goaway(_)) {
						return Ok(());
					}

					// Two peers that each hold a session past the healthy bar can still bounce us between
					// them forever. Returning to an endpoint that already sent us elsewhere in this
					// run of migrations skips the healthy-session reset, so the backoff escalates and
					// the retry window still applies. A one-way rolling restart never revisits one,
					// and a GOAWAY back to the same place is a restart, not a move, so it never
					// counts. A session that outlived the capped delay already redials no faster
					// than the backoff would allow, so it starts a new run: rolling restarts days
					// apart never add up to a loop.
					if matches!(ended, Ended::Closed(_)) || tenure >= pacing.settled() {
						redirects.reset();
					}
					// Only an accepted redirect is a move: an empty or ignored one reconnects in place.
					let looped = assigned.as_ref().is_some_and(|target| {
						let from = endpoint(&url);
						let to = endpoint(target);
						to != from && redirects.depart(from, &to)
					});

					// A session that stayed up past the healthy bar is healthy; one that
					// ended sooner, or carried on a redirect loop, counts as a failed attempt.
					let healthy = !looped && retry.settle(tenure, &pacing);

					if let Ended::Goaway(msg) = &ended {
						// An accepted redirect is an assignment: keep dialing it from here on, and
						// only it. The peer named exactly one place to go, which retires
						// whatever other addresses got us to this session.
						let url = if let Some(target) = assigned {
							addrs = Addrs::new(target.clone());
							target
						} else {
							url
						};

						// Hand over gracefully however the backoff bookkeeping scores this
						// session. The old one keeps serving until it closes or overstays,
						// so its routes stay attached and live tracks splice onto the
						// replacement at a group boundary. Tearing it down here instead
						// would drop every group published until the replacement caught up.
						tracing::info!(peer = %Endpoint(&url), "upstream GOAWAY; migrating");
						// Retire any predecessor first: overwriting would drop its deadline
						// on the floor and leave it holding the connection open.
						if let Some(mut old) = draining.take() {
							old.retire();
						}
						draining = Some(Draining::new(session, goaway.handover(msg.timeout()), &shared.state));
						// After the predecessor is published, so a close woken by this
						// status finds it and honors its handover deadline.
						shared.migrating();

						// No backoff sleep: a handover off a healthy session is not a failure.
						if healthy {
							continue;
						}

						// Drained almost immediately, or looped back. The backoff still escalates, so
						// the redials cannot hammer the peer, and the old session serves across the
						// sleep.
						//
						// An empty URI is "reconnect to me": the peer is restarting and drains
						// each new session until it goes, which is no failure, since every one
						// serves. A named URI is a redirect, scored as a failed attempt so two
						// peers bouncing us between them eventually give up.
						let restarting = msg.uri().is_empty();
						if !restarting {
							let reason = if looped {
								"redirect loop"
							} else {
								"peer redirected immediately"
							};
							let err = Error::Reconnect(reason.to_string());
							shared.retrying(&err);
							retry.last_error = Some(err);
						}
						// A restart's pacing is not capped by the window: a delay longer than the
						// window would otherwise spend it all and end the loop without a redial.
						let window = if restarting { Duration::ZERO } else { timeout };
						let Some(wait) = retry_wait(retry.delay, retry.start, window) else {
							return Err(timeout_error(timeout, retry.last_error.as_ref()));
						};
						if looped {
							tracing::warn!(peer = %Endpoint(&url), ?wait, "peer redirected back in a loop; retrying after backoff");
						} else {
							tracing::warn!(peer = %Endpoint(&url), ?wait, "peer drained the session immediately; retrying after backoff");
						}
						// Keep the handover bounded across the sleep: nothing else polls the
						// predecessor while the loop is between connections.
						sleep_draining(wait, &mut draining, shared).await;
						retry.delay = pacing.next(retry.delay);
						if restarting {
							// After the sleep, so the window counts only the time after the
							// peer stops answering.
							retry.start = moq_clock::now();
						}
						continue;
					}

					// Both returns below leave the offline bookkeeping to the terminal
					// snapshot, so they never publish a `Disconnected` first.
					//
					// Refusals and malformed protocol input are settled failures, however long
					// the session lived. A new connection cannot repair either one.
					if let Ended::Closed(Err(err)) = &ended
						&& err.is_terminal()
						&& !refreshed
					{
						return Err(err.clone());
					}

					// One-shot mode: the session ending ends the connection. Its close
					// reason is the terminal error, mirroring `moq_net::Session::closed`.
					// A GOAWAY already returned above.
					if !client.reconnect {
						return match ended {
							Ended::Closed(res) => res,
							Ended::Goaway(_) => unreachable!("handled before the migration branch"),
						};
					}

					// Publish the lost transport and why, in one lifecycle observation.
					let failure = match &ended {
						Ended::Closed(Err(err)) => Some(err),
						_ => None,
					};
					shared.offline(failure);

					if healthy {
						// The backoff window was reset, so a one-off drop reconnects promptly.
						tracing::warn!(peer = %Endpoint(&url), "session closed, reconnecting");
					} else {
						// Connected then dropped almost immediately (e.g. the server accepts then
						// resets, or redirects us straight back out). Treat it as a failed
						// connection: keep the reason so the give-up timeout reports a real cause,
						// and fall through to the shared backoff sleep below so repeated flaps
						// escalate instead of spinning the CPU.
						let err = match ended {
							Ended::Closed(Err(err)) => Some(err),
							Ended::Closed(Ok(())) => None,
							// Handled above: a GOAWAY never reaches here.
							Ended::Goaway(_) => None,
						};
						// NOTE: only UNAUTHORIZED is specified, and it is handled above. Any
						// other MoQ-layer rejection (Request::reject after the transport is
						// accepted) lands here as an untyped transport close, so it cannot be
						// told apart from a network blip and is retried until the give-up
						// timeout. Classifying the rest needs the transport to surface the
						// close code; until then, one-shot mode (`reconnect = false`) is how
						// a caller observes those rejections directly.
						match err {
							Some(err) => {
								tracing::warn!(peer = %Endpoint(&url), %err, "session severed immediately, retrying");
								retry.last_error = Some(err);
							}
							None => tracing::warn!(peer = %Endpoint(&url), "session severed immediately, retrying"),
						}
					}
				}
				Err(err) => {
					if (err.is_terminal() && !refreshed(&err, &shared.tokens, presented.as_ref())) || !client.reconnect
					{
						return Err(err);
					}
					shared.retrying(&err);
					retry.last_error = Some(err);
				}
			}

			let Some(wait) = retry_wait(retry.delay, retry.start, timeout) else {
				return Err(timeout_error(timeout, retry.last_error.as_ref()));
			};
			// No URL here: with several candidates there isn't one to name, and each
			// attempt already logged the address it tried.
			tracing::warn!(?wait, "reconnecting after backoff");
			// Drain-aware: a GOAWAY off a healthy session continues straight to the
			// replacement dial, so a predecessor can still be draining when that dial
			// fails and lands here. A plain sleep would stop enforcing its handover
			// until some later dial succeeded, holding an upstream that asked to drain
			// open for the whole retry window, or forever with `--connect-backoff-timeout=0`.
			sleep_draining(wait, &mut draining, shared).await;
			retry.delay = pacing.next(retry.delay);
		}
	}

	/// Try each address in turn, returning the first session that connects and the
	/// address that produced it.
	///
	/// A peer discovered rather than configured can advertise several addresses,
	/// only some of which route from here (its loopback, a container bridge, an
	/// interface on another subnet). Nothing in the record says which, so every
	/// attempt walks the whole list rather than pinning whichever sorted first.
	///
	/// A predecessor left over from a migration keeps draining across the whole
	/// walk, not just one attempt: its handover deadline is wall-clock, and a walk
	/// past several black-holed addresses is exactly when it would overstay.
	async fn dial_any(
		shared: &Shared,
		client: &Client,
		addrs: &Addrs,
		draining: &mut Option<Draining>,
		presented: &mut Option<moq_net::auth::Dial>,
	) -> crate::Result<(crate::connect::Addr, crate::client::Dialed)> {
		let candidates = addrs.as_slice();
		let mut last = None;
		let mut transient = None;

		for (index, addr) in candidates.iter().enumerate() {
			let url = addr.url();
			tracing::info!(peer = %Endpoint(url), "connecting");

			// Held before the dial starts its session's driver, so a request the session
			// sends early waits on their answers rather than being refused outside the
			// connection credential's grant.
			let presenting = presented.insert(shared.tokens.dial(url, &client.versions));
			let mut dial = std::pin::pin!(client.dial(addr.clone(), presenting));
			let dialed = kio::wait(|waiter| {
				if poll_draining(draining, waiter) {
					shared.disconnected();
				}
				waiter.poll_future(dial.as_mut())
			});

			let dialed = match attempt_timeout(index, candidates.len()) {
				None => dialed.await,
				Some(limit) => match moq_clock::timeout(limit, dialed).await {
					Ok(dialed) => dialed,
					Err(_) => Err(Error::Reconnect(format!("timed out connecting to {}", Endpoint(url)))),
				},
			};

			match dialed {
				Ok(dialed) => return Ok((addr.clone(), dialed)),
				// A status the peer actually sent is its answer, not this address's, so
				// unless it invites another attempt it settles the whole walk. Carrying
				// on would offer the same rejected credentials at the peer's other
				// addresses, and worse, a later transport failure would overwrite the
				// real reason and leave the outer loop retrying until its budget ran out.
				Err(err) if settles_walk(&err) => return Err(err),
				Err(err) => {
					tracing::debug!(peer = %Endpoint(url), %err, "address unreachable");
					if !err.is_terminal() {
						transient = Some(err.clone());
					}
					last = Some(err);
				}
			}
		}

		// One unusable candidate must not turn another candidate's transient failure terminal.
		Err(transient.or(last).expect("Addrs is never empty"))
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

/// The reconnect loop's backoff window across a run of failed attempts.
struct Retry {
	/// The wait before the next attempt, before jitter.
	delay: Duration,
	/// When this run of failures began; the give-up timeout counts from here.
	start: moq_clock::Instant,
	/// The latest failure, so giving up reports a real cause.
	last_error: Option<Error>,
}

impl Retry {
	fn new(initial: Duration) -> Self {
		Self {
			delay: initial,
			start: moq_clock::now(),
			last_error: None,
		}
	}

	/// Score how long a session served: past the healthy bar starts a fresh retry window.
	fn settle(&mut self, tenure: Duration, pacing: &Pacing) -> bool {
		let healthy = tenure >= pacing.healthy;
		if healthy {
			*self = Self::new(pacing.initial);
		}
		healthy
	}
}

/// Build the terminal error for an exhausted retry window without discarding the last real cause.
fn timeout_error(timeout: Duration, last_error: Option<&Error>) -> Error {
	let message = match last_error {
		Some(err) => format!("reconnect timed out after {timeout:?}: {err}"),
		None => format!("reconnect timed out after {timeout:?}"),
	};
	Error::Reconnect(message)
}

/// Jitter the next delay and cap it at the retry window. `None` means the window is exhausted.
fn retry_wait(delay: Duration, retry_start: moq_clock::Instant, timeout: Duration) -> Option<Duration> {
	let wait = delay.mul_f64(0.5 + rand::rng().random::<f64>() / 2.0);
	if timeout.is_zero() {
		return Some(wait);
	}
	Some(wait.min(timeout.checked_sub(retry_start.elapsed())?))
}

/// Where `url` dials, without its userinfo, query, or fragment, where a peer may mint fresh
/// credentials on every redirect. Two URLs that differ only there reach the same endpoint.
fn endpoint(url: &Url) -> Url {
	let mut bare = url.clone();
	// Only fails for a URL that cannot carry userinfo, which then has none to strip.
	let _ = bare.set_username("");
	let _ = bare.set_password(None);
	bare.set_query(None);
	bare.set_fragment(None);
	bare
}

/// A run of GOAWAY redirects, and whether it has revisited an endpoint.
///
/// The peer picks every hop, so remembering them all would grow without bound. Brent's cycle
/// detection instead keeps one departed endpoint, replaced at doubling intervals, and catches a
/// cycle of any length within a few laps of it. Once caught, the run stays a loop until reset.
struct Redirects {
	mark: Option<Url>,
	span: u64,
	hops: u64,
	looped: bool,
}

impl Default for Redirects {
	fn default() -> Self {
		Self {
			mark: None,
			span: 1,
			hops: 0,
			looped: false,
		}
	}
}

impl Redirects {
	/// Record leaving `from` for a different endpoint `to`, and return whether this run loops.
	fn depart(&mut self, from: Url, to: &Url) -> bool {
		if self.mark.as_ref() == Some(to) {
			self.looped = true;
		}
		self.hops += 1;
		if self.hops == self.span {
			self.mark = Some(from);
			self.span = self.span.saturating_mul(2);
			self.hops = 0;
		}
		self.looped
	}

	/// Start a new run.
	fn reset(&mut self) {
		*self = Self::default();
	}
}

/// Why a session stopped being the live one.
enum Ended {
	/// The transport closed.
	Closed(crate::Result<()>),
	/// The peer sent a GOAWAY; the session is still up and serving.
	Goaway(moq_net::goaway::Goaway),
}

/// An old session kept alive after a GOAWAY so its in-flight groups finish.
///
/// Held by the reconnect loop rather than a detached task, so dropping the
/// [`Connection`] handle tears the old session down with everything else instead
/// of leaving an orphan holding the connection open.
struct Draining {
	session: moq_net::Session,
	closed: std::pin::Pin<Box<dyn Future<Output = ()> + Send>>,
	deadline: std::pin::Pin<Box<moq_clock::Sleep>>,
	/// Mirrors the session into [`State::predecessor`] for as long as this lives.
	state: kio::Producer<State>,
}

impl Draining {
	fn new(session: moq_net::Session, handover: Duration, state: &kio::Producer<State>) -> Self {
		let deadline = Box::pin(moq_clock::sleep(handover));
		if let Ok(mut state) = state.write() {
			state.predecessor = Some((session.clone(), deadline.deadline()));
		}
		let closed = {
			let session = session.clone();
			Box::pin(async move {
				session.closed().await;
			}) as std::pin::Pin<Box<dyn Future<Output = ()> + Send>>
		};

		Self {
			session,
			closed,
			deadline,
			state: state.clone(),
		}
	}

	/// Close the old session now, whatever remains of its window.
	fn retire(&mut self) {
		self.session.abort(moq_net::Error::GoawayTimeout);
	}

	/// Poll the drain; `true` once it is over and the handle should be released.
	fn poll(&mut self, waiter: &kio::Waiter) -> bool {
		if waiter.poll_future(self.closed.as_mut()).is_ready() {
			tracing::debug!("old session drained cleanly after GOAWAY");
			return true;
		}
		if waiter.poll_future(self.deadline.as_mut()).is_ready() {
			tracing::warn!("old session did not drain in time; closing");
			self.session.abort(moq_net::Error::GoawayTimeout);
			return true;
		}
		false
	}
}

impl Drop for Draining {
	fn drop(&mut self) {
		if let Ok(mut state) = self.state.write() {
			state.predecessor = None;
		}
	}
}

/// Poll a draining predecessor, retiring it once it closes or overstays its
/// handover window. `true` on the pass that retires it.
///
/// A `poll_*` step: on return the waiter is registered for the next change. The
/// caller decides what the retirement means, because that depends on whether a
/// replacement is already serving.
fn poll_draining(draining: &mut Option<Draining>, waiter: &kio::Waiter) -> bool {
	if let Some(old) = draining.as_mut()
		&& old.poll(waiter)
	{
		*draining = None;
		return true;
	}
	false
}

/// Sleep for `delay` while keeping a draining predecessor's deadline enforced.
///
/// The drain is otherwise only polled from [`run_session`], which is reached only
/// after a successful connect, so a replacement that takes several backoff rounds
/// to reach would leave the old session holding its connection well past the
/// handover window.
async fn sleep_draining(delay: Duration, draining: &mut Option<Draining>, shared: &Shared) {
	let mut sleep = std::pin::pin!(moq_clock::sleep(delay));
	kio::wait(|waiter| {
		// Nothing is serving once it retires: the predecessor closed or overstayed
		// its handover, and the replacement is still a dial away. Leaving the state
		// on `Migrating` would keep handing callers a session that is already closed,
		// with its stats and version, until the next connect overwrote it.
		if poll_draining(draining, waiter) {
			shared.disconnected();
		}
		waiter.poll_future(sleep.as_mut())
	})
	.await
}

/// Wait for `session` to close or receive a GOAWAY, forwarding its send/recv bandwidth estimates
/// into the persistent producers meanwhile so [`Connection`] consumers track the live estimates
/// across the connection, and draining any predecessor left over from an earlier migration.
///
/// One `poll_*` step drives it all: [`poll_forward`] mirrors each kio bandwidth estimate, the
/// GOAWAY consumer is a kio channel, and the transport's close future (the one non-kio source) is
/// polled through the waiter's own waker.
///
/// Under [`Restart::Wait`] a GOAWAY naming no URI does not end the wait: the session serves on
/// until it closes.
async fn run_session(
	shared: &Shared,
	session: &moq_net::Session,
	draining: &mut Option<Draining>,
	restart: Restart,
) -> Ended {
	let mut send = session.send_bandwidth();
	let mut recv = RecvBandwidth::default();
	// Dropped once a restart is waited out: it is a latch, so it would keep firing.
	let mut goaway = Some(session.draining());
	let closed = session.closed();
	tokio::pin!(closed);

	kio::wait(|waiter| {
		poll_forward(&mut send, &shared.send_bw, waiter);
		recv.poll(&shared.recv_bw, waiter, || session.recv_bandwidth());

		// Retire the predecessor once it closes or overstays its handover window.
		// The replacement is already serving, so the state stays as it is.
		poll_draining(draining, waiter);

		// Checked before the close arm: a GOAWAY means the session is still up, and
		// migrating from it is not the same as reconnecting after it died.
		//
		// Polled in one-shot mode too, even though there is no replacement to dial.
		// A GOAWAY naming no deadline never force-closes, and the peer stops
		// accepting requests and waits for us to leave, so ignoring it is how both
		// sides end up waiting on each other forever. The caller ends the connection
		// instead: asked to leave, with no way to migrate, leaving is the answer.
		if let Some(Poll::Ready(Ok(msg))) = goaway.as_ref().map(|goaway| goaway.poll(waiter)) {
			if restart == Restart::Wait && msg.uri().is_empty() {
				tracing::info!("peer is restarting; serving until it closes the session");
				goaway = None;
			} else {
				return Poll::Ready(Ended::Goaway(msg));
			}
		}

		waiter
			.poll_future(closed.as_mut())
			.map(|err| Ended::Closed(Err(err.into())))
	})
	.await
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

		// With no flags, the parser defaults resolve to the typed defaults.
		let parsed = Wrapper::parse_from(&[]).unwrap();
		assert_eq!(parsed.backoff.multiplier, DEFAULT_MULTIPLIER);
		let pacing = Pacing::new(&parsed.backoff);
		assert_eq!(pacing.initial, DEFAULT_INITIAL);
		assert_eq!(pacing.max, Some(DEFAULT_MAX));
		assert_eq!(pacing.healthy, DEFAULT_HEALTHY);
		assert_eq!(parsed.backoff.timeout(), DEFAULT_TIMEOUT);

		// And a flag still lands where the merge can see it.
		let parsed = Wrapper::parse_from(&[
			std::ffi::OsStr::new("--connect-backoff-initial"),
			std::ffi::OsStr::new("3s"),
		])
		.unwrap();
		assert_eq!(Pacing::new(&parsed.backoff).initial, Duration::from_secs(3));
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
		let resolved = parsed.goaway.resolve();
		assert_eq!(resolved.redirect, Redirect::SameHost);
		assert_eq!(resolved.handover(None), Duration::from_secs(10));

		// Flags passed: they land where the merge can see them.
		let parsed = Wrapper::parse_from(&[
			std::ffi::OsStr::new("--connect-goaway-redirect"),
			std::ffi::OsStr::new("ignore"),
			std::ffi::OsStr::new("--connect-goaway-handover"),
			std::ffi::OsStr::new("3s"),
		])
		.unwrap();
		assert_eq!(parsed.goaway.redirect, Redirect::Ignore);
		assert_eq!(parsed.goaway.resolve().handover(None), Duration::from_secs(3));
	}

	/// Our own window caps the peer's. A GOAWAY deadline shortens the handover but
	/// never extends it, so an upstream naming an absurd one cannot make us hold a
	/// dying session open for it.
	#[test]
	fn handover_takes_the_earlier_deadline() {
		let config = Goaway {
			handover: Some((Duration::from_secs(10)).into()),
			..Default::default()
		};

		let config = config.resolve();
		assert_eq!(config.handover(None), Duration::from_secs(10), "no deadline: ours");
		assert_eq!(
			config.handover(Some(Duration::from_secs(3))),
			Duration::from_secs(3),
			"a shorter peer deadline wins: it force-closes then anyway"
		);
		assert_eq!(
			config.handover(Some(Duration::from_secs(3600))),
			Duration::from_secs(10),
			"a longer peer deadline must not extend our cap"
		);

		// The default is a cap too, not just a fallback for a silent peer.
		let default = Goaway::default().resolve();
		assert_eq!(default.handover(Some(Duration::from_secs(3600))), DEFAULT_HANDOVER);
	}

	/// A peer must not be able to redirect us somewhere we could not already
	/// reach. IPv4-mapped and link-local forms are the easy ones to miss.
	#[test]
	fn local_targets_are_recognized() {
		let local = [
			"https://127.0.0.1/",
			"https://localhost/",
			"https://[::1]/",
			"https://10.0.0.1/",
			"https://169.254.1.1/",
			"https://[::ffff:127.0.0.1]/",
			"https://[::ffff:10.0.0.1]/",
			"https://[fe80::1]/",
			"https://[fc00::1]/",
			"https://0.0.0.0/",
		];
		for url in local {
			assert!(is_local(&url.parse().unwrap()), "{url} should be local");
		}

		for url in ["https://example.com/", "https://8.8.8.8/", "https://[2606:4700::1]/"] {
			assert!(!is_local(&url.parse().unwrap()), "{url} should not be local");
		}

		// A public endpoint may not redirect us inward, but a local one may stay local.
		let public: Url = "https://relay.example/".parse().unwrap();
		let localhost: Url = "https://127.0.0.1:4443/".parse().unwrap();
		assert!(matches!(
			Redirect::Follow.target("https://[::ffff:127.0.0.1]/", &public, false),
			Err(Error::RefusedRedirect(_))
		));
		assert_eq!(
			Redirect::Follow
				.target("https://127.0.0.1:9999/", &localhost, false)
				.unwrap()
				.unwrap()
				.port(),
			Some(9999)
		);
	}

	/// The scheme ranking is what stops a peer from quietly moving us off an
	/// encrypted transport. An unknown scheme ranks lowest so a classification we
	/// forgot to add is refused rather than trusted.
	#[test]
	fn scheme_tiers_rank_encrypted_above_plaintext() {
		for scheme in ["https", "moqt", "moql", "wss", "iroh", "tls"] {
			assert_eq!(scheme_tier(scheme), 2, "{scheme} is encrypted");
		}
		for scheme in ["tcp", "ws", "http"] {
			assert_eq!(scheme_tier(scheme), 1, "{scheme} is plaintext");
		}
		// `unix` is not an upgrade over a network transport, it is a different
		// reachability class, so it must not outrank one.
		for scheme in ["unix", "gopher", ""] {
			assert_eq!(scheme_tier(scheme), 0, "{scheme} is unclassified");
		}
	}

	/// A redirect may hold the scheme or improve it, never weaken it. A downgrade
	/// is a refusal, not a reason to redial the address the peer is leaving.
	#[test]
	fn target_refuses_a_scheme_downgrade() {
		let secure: Url = "https://relay.example/".parse().unwrap();
		let plain: Url = "http://relay.example/".parse().unwrap();

		assert!(matches!(
			Redirect::Follow.target("http://other.example/", &secure, false),
			Err(Error::RefusedRedirect(_))
		));
		assert!(matches!(
			Redirect::Follow.target("unix:///tmp/moq.sock", &secure, false),
			Err(Error::RefusedRedirect(_))
		));

		// Same tier and upgrades are followed.
		let same: Url = "https://other.example/".parse().unwrap();
		assert_eq!(
			Redirect::Follow
				.target("https://other.example/", &secure, false)
				.unwrap(),
			Some(same.clone())
		);
		assert_eq!(
			Redirect::Follow
				.target("https://other.example/", &plain, false)
				.unwrap(),
			Some(same)
		);
	}

	/// Native schemes must classify address literals even when the URL parser returns a domain.
	#[test]
	fn a_non_special_scheme_classifies_its_literal_host() {
		for url in [
			"moqt://127.0.0.1/",
			"moql://10.0.0.1/",
			"tcp://169.254.169.254/",
			"moqt://[::1]/",
			"moqt://[::ffff:127.0.0.1]/",
		] {
			assert!(is_local(&url.parse().unwrap()), "{url} should be local");
		}
		for url in ["moqt://8.8.8.8/", "moql://[2606:4700::1]/", "tcp://relay.example/"] {
			assert!(!is_local(&url.parse().unwrap()), "{url} should not be local");
		}

		let public: Url = "moqt://relay.example/".parse().unwrap();
		assert!(
			matches!(
				Redirect::Follow.target("moqt://169.254.169.254/", &public, false),
				Err(Error::RefusedRedirect(_))
			),
			"a literal local target is refused whatever the scheme"
		);
	}

	/// The reachability guard reads the URL, so it cannot see a local address
	/// hiding behind a hostname: any name the peer controls can carry an A record
	/// for loopback or a private range, and a second lookup at dial time may answer
	/// differently anyway. The default policy is what closes that, by refusing to
	/// let a peer name a host at all.
	#[test]
	fn a_peer_named_host_is_refused_by_default() {
		let public: Url = "https://relay.example/".parse().unwrap();

		// The bypass: a name is not local on its face, so the reachability guard
		// passes it through whatever it resolves to.
		let rebindable: Url = "https://rebind.attacker.example/".parse().unwrap();
		assert!(!is_local(&rebindable), "a hostname is never classified as local");
		assert_eq!(
			Redirect::Follow.target(rebindable.as_str(), &public, false).unwrap(),
			Some(rebindable.clone()),
			"Follow is explicit trust: it dials the name the peer chose"
		);

		// So the default refuses the host change instead of trying to judge it.
		assert_eq!(Redirect::default(), Redirect::SameHost);
		assert!(
			matches!(
				Redirect::default().target(rebindable.as_str(), &public, false),
				Err(Error::RefusedRedirect(_))
			),
			"a peer-named host is refused without resolving it"
		);
		assert_eq!(
			Goaway::default().resolve().redirect,
			Redirect::SameHost,
			"and the shipped config carries that default"
		);
	}

	/// Only an explicit URI can assign, and one the policy will not follow is an
	/// error rather than a quiet fallback: the loop ends on it instead of redialing.
	#[test]
	fn only_an_accepted_redirect_replaces_the_address_list() {
		let current: Url = "https://relay.example/".parse().unwrap();

		// No URI, or a policy that ignores it: keep the current address list.
		assert_eq!(Redirect::Follow.target("", &current, false).unwrap(), None);
		assert_eq!(
			Redirect::Ignore
				.target("https://relay.example:5443/", &current, false)
				.unwrap(),
			None
		);

		// An explicit URI the policy will not follow is refused.
		for (policy, uri) in [
			(Redirect::SameHost, "https://other.example/"),
			(Redirect::Follow, "not a url"),
			(Redirect::Ignore, "not a url"),
			(Redirect::Follow, "http://relay.example/"),
			(Redirect::Follow, "https://127.0.0.1/"),
		] {
			assert!(
				matches!(policy.target(uri, &current, false), Err(Error::RefusedRedirect(_))),
				"{policy:?}: {uri}"
			);
		}

		// An explicit assignment remains an assignment even if its URL is unchanged.
		assert_eq!(
			Redirect::SameHost.target(current.as_str(), &current, false).unwrap(),
			Some(current.clone())
		);
		let moved = "https://relay.example:5443/";
		assert_eq!(
			Redirect::SameHost.target(moved, &current, false).unwrap(),
			Some(moved.parse().unwrap())
		);
	}

	/// A fingerprint is a Rustls check. Schemes that never install that verifier
	/// must not inherit the pin.
	#[test]
	fn a_fingerprint_pin_only_covers_rustls_schemes() {
		for scheme in ["https", "wss", "moqt", "moql", "tls"] {
			assert!(fingerprint_pins(scheme), "{scheme}");
		}
		for scheme in ["http", "ws", "tcp", "unix", "iroh"] {
			assert!(!fingerprint_pins(scheme), "{scheme}");
		}
	}

	/// A certificate pin verifies only the host it was configured for, so it
	/// refuses a host change even when the policy would follow one.
	#[test]
	fn a_certificate_pin_holds_the_host() {
		let current: Url = "https://relay.example/".parse().unwrap();
		assert!(matches!(
			Redirect::Follow.target("https://other.example/", &current, true),
			Err(Error::RefusedRedirect(_))
		));
		let moved = "https://relay.example:5443/";
		assert_eq!(
			Redirect::Follow.target(moved, &current, true).unwrap(),
			Some(moved.parse().unwrap())
		);
	}

	/// `SameHost` lets a peer move us between ports or schemes on the endpoint we
	/// already chose, but not onto a different host.
	#[test]
	fn same_host_follows_a_port_not_a_host() {
		let current: Url = "https://relay.example:4443/".parse().unwrap();

		assert!(
			matches!(
				Redirect::SameHost.target("https://elsewhere.example/", &current, false),
				Err(Error::RefusedRedirect(_))
			),
			"another host is refused"
		);

		let moved = Redirect::SameHost
			.target("https://relay.example:5443/", &current, false)
			.unwrap()
			.unwrap();
		assert_eq!(
			moved.port(),
			Some(5443),
			"a different port on the same host is followed"
		);
	}

	/// Each knob means one thing at zero: no wait, no cap, no growth. None of them touches
	/// the healthy bar, which is what keeps an immediate close a failed attempt.
	#[test]
	fn zero_turns_each_pacing_knob_off() {
		let pacing = Pacing::new(&Backoff {
			initial: Some((Duration::ZERO).into()),
			..Default::default()
		});
		assert_eq!(pacing.initial, Duration::ZERO);
		assert_eq!(pacing.next(pacing.initial), Duration::ZERO, "zero initial never waits");
		assert_eq!(pacing.healthy, DEFAULT_HEALTHY);

		let pacing = Pacing::new(&Backoff {
			max: Some((Duration::ZERO).into()),
			..Default::default()
		});
		assert_eq!(pacing.max, None);
		let mut delay = pacing.initial;
		for _ in 0..10 {
			delay = pacing.next(delay);
		}
		assert_eq!(delay, DEFAULT_INITIAL * 1024, "zero max leaves the delay uncapped");
		assert_eq!(pacing.settled(), DEFAULT_HEALTHY);

		let pacing = Pacing::new(&Backoff {
			multiplier: 0,
			..Default::default()
		});
		assert_eq!(
			pacing.next(pacing.initial),
			pacing.initial,
			"zero multiplier never grows"
		);

		// A cap below the first delay caps it too.
		let pacing = Pacing::new(&Backoff {
			max: Some((Duration::from_millis(300)).into()),
			..Default::default()
		});
		assert_eq!(pacing.initial, Duration::from_millis(300));
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

	/// A sane config is left alone: the delay doubles up to the cap and stays there.
	#[test]
	fn pacing_grows_to_the_cap() {
		let pacing = Pacing::new(&Backoff {
			initial: Some((Duration::from_millis(100)).into()),
			multiplier: 2,
			max: Some((Duration::from_millis(400)).into()),
			..Default::default()
		});

		assert_eq!(pacing.initial, Duration::from_millis(100));
		assert_eq!(pacing.settled(), Duration::from_millis(400));
		assert_eq!(pacing.next(Duration::from_millis(100)), Duration::from_millis(200));
		assert_eq!(pacing.next(Duration::from_millis(200)), Duration::from_millis(400));
		assert_eq!(pacing.next(Duration::from_millis(400)), Duration::from_millis(400));
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
	async fn dial_any_walks_past_the_dead_addresses() {
		let client = client();
		let (mut server, live) = live().await;
		tokio::spawn(async move { while server.accept().await.is_some() {} });

		let dead_a = dead().await;
		let dead_b = dead().await;
		let addrs = Addrs::collect([dead_a, dead_b, live.clone()]).expect("not empty");
		let shared = shared();
		let mut draining = None;

		let (connected, _session) = moq_clock::timeout(
			WALK_TIMEOUT,
			Connection::dial_any(&shared, &client, &addrs, &mut draining, &mut None),
		)
		.await
		.expect("the walk must not hang")
		.expect("the live address must connect");
		assert_eq!(connected.url(), &live, "the walk must land on the one that answers");
	}

	/// With nothing reachable the walk reports a failure rather than hanging.
	#[cfg(feature = "tcp")]
	#[tokio::test]
	async fn dial_any_reports_failure_when_nothing_answers() {
		let client = client();

		let dead_a = dead().await;
		let dead_b = dead().await;
		let addrs = Addrs::collect([dead_a, dead_b]).expect("not empty");
		let shared = shared();
		let mut draining = None;

		let result = moq_clock::timeout(
			WALK_TIMEOUT,
			Connection::dial_any(&shared, &client, &addrs, &mut draining, &mut None),
		)
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
	/// it.
	#[cfg(feature = "tcp")]
	#[tracing_test::traced_test]
	#[tokio::test]
	async fn dialing_never_logs_the_credential() {
		const SECRET: &str = "b91d7fe20c4a";

		let client = client();
		let mut target = dead().await;
		target.set_path(&format!("/.cluster/{SECRET}"));

		let addrs = Addrs::new(target);
		let shared = shared();
		let mut draining = None;
		// `Session` isn't `Debug`, so unwrap the failure by hand.
		let Err(err) = Connection::dial_any(&shared, &client, &addrs, &mut draining, &mut None).await else {
			panic!("a dead address must fail");
		};

		// The log line did happen, so the assertion below isn't vacuously true.
		assert!(logs_contain("connecting"), "the dial never logged at all");
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
		let started = moq_clock::now();
		let connection = client.connect(url("moqz://localhost/"));
		let err = moq_clock::timeout(Duration::from_secs(1), connection.closed())
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
			let err = moq_clock::timeout(Duration::from_secs(1), connection.closed())
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

		let started = moq_clock::now();
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
			let mut request = moq_clock::timeout(WAIT, listener.accept()).await.unwrap().unwrap();
			let mut requests = request.requests();
			let session = request.ok().await.unwrap();
			loop {
				let request = moq_clock::timeout(WAIT, requests.next()).await.unwrap().unwrap();
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
				moq_clock::timeout(WAIT, grant.changed()).await.unwrap().unwrap();
			}
			session.abort(moq_net::Error::Cancel);
		}
		drop(connection);
		let token = token.unwrap();
		// Not `Cancel`, which reads as the peer ending the grant cleanly.
		let closed = moq_clock::timeout(WAIT, token.closed()).await.unwrap();
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

		let session = moq_clock::timeout(WAIT, accepted.recv()).await.unwrap().unwrap();
		connection.auth().abort(moq_net::Error::App(9));
		session.abort(moq_net::Error::Cancel);
		let closed = moq_clock::timeout(WAIT, connection.closed())
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

		let session = moq_clock::timeout(WAIT, accepted.recv()).await.unwrap().unwrap();
		let mut state = monitor.state();
		while state.phase != Phase::Connected {
			state = moq_clock::timeout(WAIT, monitor.state_changed(state.revision))
				.await
				.unwrap()
				.unwrap();
		}
		let (woken, _waiter) = Woken::watch(&monitor);
		session.abort(moq_net::Error::App(7));
		let closed = moq_clock::timeout(WAIT, connection.closed())
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
		assert!(settles_walk(&Error::Connect(crate::ConnectError::Unauthorized)));
		assert!(settles_walk(&Error::Connect(crate::ConnectError::Forbidden)));
		assert!(settles_walk(&Error::MoqNet(moq_net::Error::ProtocolViolation)));
		assert!(!settles_walk(&Error::MoqNet(moq_net::Error::Closed)));
		assert!(!settles_walk(&Error::Io(std::sync::Arc::new(
			std::io::ErrorKind::ConnectionRefused.into()
		))));
	}

	/// Only an attempt with somewhere to fall back on gets the fixed bound.
	#[test]
	fn only_a_candidate_with_a_fallback_is_bounded() {
		assert_eq!(attempt_timeout(0, 1), None, "a lone address");
		assert_eq!(attempt_timeout(0, 2), Some(CONNECT_ATTEMPT), "one more to try");
		assert_eq!(attempt_timeout(1, 2), None, "the last of two");
		assert_eq!(attempt_timeout(1, 3), Some(CONNECT_ATTEMPT), "still one more");
		assert_eq!(attempt_timeout(2, 3), None, "the last of three");
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
			while let Some(request) = server.accept().await {
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
		let connection = moq_clock::timeout(WAIT, client.connect(url).established())
			.await
			.expect("never connected")
			.unwrap();
		assert_eq!(connection.transport(), Some(crate::Transport::WebTransport));
		let (transport, _session) = moq_clock::timeout(WAIT, sessions.recv())
			.await
			.expect("the server never accepted a session")
			.unwrap();
		assert_eq!(transport, crate::Transport::WebTransport);

		moq_clock::timeout(WAIT, cancelled)
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
			while let Some(request) = server.accept().await {
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

		let session_a = moq_clock::timeout(WAIT, accepted_a.recv()).await.unwrap().unwrap();

		let consumer = subscriber.consume();
		let cam = moq_clock::timeout(WAIT, consumer.follow("cam", None).unwrap().available())
			.await
			.unwrap()
			.unwrap();
		let mut sub = cam.track("video").unwrap().subscribe(None).await.unwrap();

		let mut group = track.append_group().unwrap();
		group.write_frame(moq_net::Timestamp::ZERO, b"g0".as_ref()).unwrap();
		group.finish().unwrap();
		let g0 = moq_clock::timeout(WAIT, sub.recv_group())
			.await
			.unwrap()
			.unwrap()
			.unwrap();
		assert_eq!(g0.sequence, 0);

		// Withdraw A from DNS, then drain it.
		crate::resolve::hosts::point(HOST, [addr_b]);
		let drained = moq_clock::now();
		session_a
			.drain()
			.send(moq_net::goaway::Goaway::new().with_timeout(DEADLINE))
			.unwrap();

		let _session_b = moq_clock::timeout(WAIT, accepted_b.recv())
			.await
			.expect("never redialed through the fresh resolve")
			.unwrap();

		let mut group = track.append_group().unwrap();
		group.write_frame(moq_net::Timestamp::ZERO, b"g1".as_ref()).unwrap();
		group.finish().unwrap();
		let mut g1 = moq_clock::timeout(WAIT, sub.recv_group())
			.await
			.unwrap()
			.unwrap()
			.unwrap();
		assert_eq!(g1.sequence, 1, "delivery resumes at the next group after the swap");
		assert_eq!(g1.read_frame().await.unwrap().unwrap().payload[..], b"g1"[..]);

		moq_clock::timeout(WAIT, session_a.closed())
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
		let status = moq_clock::timeout(WAIT, connection.status()).await.unwrap();
		assert_eq!(status.unwrap(), Status::Connected);
		let session = moq_clock::timeout(WAIT, accepted.recv()).await.unwrap().unwrap();

		session.drain().send(moq_net::goaway::Goaway::new()).unwrap();

		// A broadcast announced after the GOAWAY still arrives: the drained session serves.
		let late = origin.create_broadcast("late").unwrap();
		late.announce(Default::default()).unwrap();
		moq_clock::timeout(WAIT, subscriber.consume().follow("late", None).unwrap().available())
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
		let status = moq_clock::timeout(WAIT, connection.status()).await.unwrap();
		assert_eq!(status.unwrap(), Status::Disconnected);
		let status = moq_clock::timeout(WAIT, connection.status()).await.unwrap();
		assert_eq!(status.unwrap(), Status::Connected);
		moq_clock::timeout(WAIT, accepted.recv())
			.await
			.expect("never redialed after the close")
			.unwrap();
	}

	/// What a flapping peer does with each session the moment it is up.
	#[cfg(feature = "tcp")]
	#[derive(Clone)]
	enum Flap {
		/// Drop it, closing the session.
		Close,
		/// Send a GOAWAY naming this URI, empty for a restart, and keep serving.
		Goaway(String),
	}

	/// A client with the given pacing whose dials reach in-memory peers instead of a
	/// transport, so the paused clock only moves once every task waits on a timer.
	///
	/// The peer at each URL ends every session as its [`Flap`] says, and reports the
	/// URL and instant each session came up on the returned channel.
	#[cfg(feature = "tcp")]
	fn flapping(
		initial: Duration,
		max: Duration,
		timeout: Duration,
		peers: Vec<(Url, Flap)>,
	) -> (Client, tokio::sync::mpsc::UnboundedReceiver<(Url, moq_clock::Instant)>) {
		let mut config = crate::connect::Config::default();
		config.backoff.initial = Some(initial.into());
		config.backoff.max = Some(max.into());
		config.backoff.timeout = Some(timeout.into());
		let mut client = config.init(Default::default()).expect("build client");

		let (accepted, accepts) = tokio::sync::mpsc::unbounded_channel();
		client.dialer = Some(std::sync::Arc::new(move |addr: crate::connect::Addr| {
			let url = addr.url().clone();
			let (_, flap) = peers
				.iter()
				.find(|(peer, _)| *peer == url)
				.expect("an unknown peer")
				.clone();
			let accepted = accepted.clone();
			Box::pin(async move {
				let session = flap_once(flap).await;
				let _ = accepted.send((url, moq_clock::now()));
				Ok(crate::client::Dialed {
					session,
					transport: crate::Transport::Tcp,
				})
			})
		}));
		(client, accepts)
	}

	/// Run one in-memory MoQ handshake whose server side then ends the session as `flap` says.
	#[cfg(feature = "tcp")]
	async fn flap_once(flap: Flap) -> moq_net::Session {
		let versions = moq_net::Versions::from("moq-lite-07-wip".parse::<moq_net::Version>().unwrap());
		let config = || {
			let mut config = qmux::Config::new(qmux::Version::QMux01);
			config.protocol = qmux::Protocol::Negotiate(versions.alpns().iter().map(|alpn| alpn.to_string()).collect());
			config
		};
		let stream = |io| qmux::transport::Stream::new(io);
		let (client, server) = tokio::io::duplex(64 * 1024);
		let (client, server) = tokio::try_join!(
			qmux::Session::connect(stream(client), config()),
			qmux::Session::accept(stream(server), config())
		)
		.unwrap();
		let now = moq_net::time::Instant::now();
		let client = async {
			let (session, driver) = moq_net::Client::new()
				.with_versions(versions.clone())
				.connect(now, client)
				.await
				.unwrap();
			tokio::spawn(moq_net::time::run(driver));
			session
		};
		let server = async {
			let request = moq_net::Server::new()
				.with_versions(versions.clone())
				.accept_request(now, server)
				.await
				.unwrap();
			let (session, driver) = request.ok().await.unwrap();
			tokio::spawn(moq_net::time::run(driver));
			session
		};
		let (client, server) = tokio::join!(client, server);
		match flap {
			Flap::Close => drop(server),
			Flap::Goaway(uri) => {
				let goaway = match uri.is_empty() {
					true => moq_net::goaway::Goaway::new(),
					false => moq_net::goaway::Goaway::redirect(uri),
				};
				server.drain().send(goaway).unwrap();
				tokio::spawn(async move {
					server.closed().await;
				});
			}
		}
		client
	}

	/// Check each gap between consecutive sessions against the jittered delay it should
	/// have waited: between half of `delay` and all of it, starting at `initial` and
	/// doubling up to `max`.
	#[cfg(feature = "tcp")]
	fn assert_paced(accepts: &[(Url, moq_clock::Instant)], initial: Duration, max: Duration) {
		let mut delay = initial;
		for (index, pair) in accepts.windows(2).enumerate() {
			let gap = pair[1].1 - pair[0].1;
			assert!(
				gap >= delay / 2 && gap <= delay,
				"retry {index} waited {gap:?}, outside the jittered {delay:?}"
			);
			delay = (delay * 2).min(max);
		}
	}

	/// A configured token is held before its dial starts the session, so a broadcast
	/// published before the dial, which only that token's grant covers, waits for its
	/// answer instead of closing the session against the connection credential's grant.
	#[cfg(feature = "tcp")]
	#[tokio::test(start_paused = true)]
	async fn a_publication_waits_for_a_configured_token() {
		let shared = shared();
		let publish = crate::origin::spawn();
		let broadcast = publish.create_broadcast("room").unwrap();
		broadcast.announce(moq_net::origin::Route::default()).unwrap();
		let server_origin = crate::origin::spawn();

		let config = crate::connect::Config {
			tokens: vec!["connection".parse().unwrap(), "publish".parse().unwrap()],
			..Default::default()
		};
		let mut client = config.init(Default::default()).expect("build client");
		let auth = shared.auth.clone();
		let (publish_dial, server_dial) = (publish.clone(), server_origin.clone());
		client.dialer = Some(std::sync::Arc::new(move |_addr: crate::connect::Addr| {
			let (auth, publish, server_origin) = (auth.clone(), publish_dial.clone(), server_dial.clone());
			Box::pin(async move {
				let versions = moq_net::Versions::from("moq-lite-07-wip".parse::<moq_net::Version>().unwrap());
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
				let now = moq_net::time::Instant::now();
				let moq = moq_net::Client::new()
					.with_versions(versions.clone())
					.with_auth(auth.clone())
					.with_publisher(&publish);
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
				moq_clock::sleep(Duration::from_millis(10)).await;
				Ok(crate::client::Dialed {
					session,
					transport: crate::Transport::Tcp,
				})
			})
		}));

		let addrs = Addrs::new("moqt://relay.example.com/".parse::<Url>().unwrap());
		shared.tokens.set(client.tokens.clone());
		let (_, dialed) = Connection::dial_any(&shared, &client, &addrs, &mut None, &mut None)
			.await
			.expect("the dial failed");

		let mut announced = server_origin.consume().announced();
		let arrived = moq_clock::timeout(Duration::from_secs(5), announced.next())
			.await
			.expect("the broadcast never reached the server")
			.expect("the server's origin closed");
		assert!(matches!(arrived, moq_net::announce::Event::Start(update) if update.prefix.as_str() == "room"));
		assert!(
			futures::FutureExt::now_or_never(dialed.session.closed()).is_none(),
			"the session closed before the configured token was answered"
		);
	}

	#[cfg(feature = "tcp")]
	const INITIAL: Duration = Duration::from_millis(100);
	#[cfg(feature = "tcp")]
	const MAX: Duration = Duration::from_millis(400);

	/// A peer that accepts and then closes every session at once never earns the healthy
	/// reset: the delay escalates to the cap, and the timeout ends the loop.
	#[cfg(feature = "tcp")]
	#[tokio::test(start_paused = true)]
	async fn an_immediate_close_loop_escalates_until_the_timeout() {
		const TIMEOUT: Duration = Duration::from_secs(2);
		let peer = url("tcp://peer:1/");
		let (client, mut accepted) = flapping(INITIAL, MAX, TIMEOUT, vec![(peer.clone(), Flap::Close)]);

		let started = moq_clock::now();
		let err = client
			.connect(peer)
			.closed()
			.await
			.expect_err("a flapping peer kept the connection");
		assert!(matches!(err, Error::Reconnect(_)), "{err}");
		assert_eq!(started.elapsed(), TIMEOUT, "the timeout did not end the loop");

		let mut accepts = Vec::new();
		while let Ok(accept) = accepted.try_recv() {
			accepts.push(accept);
		}
		assert!(accepts.len() >= 6, "only {} sessions", accepts.len());
		assert_paced(&accepts, INITIAL, MAX);
	}

	/// A peer restarting with an empty-URI GOAWAY on every session paces the redials like
	/// failures, yet never spends the timeout: each drained session still served.
	#[cfg(feature = "tcp")]
	#[tokio::test(start_paused = true)]
	async fn an_immediate_restart_loop_escalates_without_giving_up() {
		const TIMEOUT: Duration = Duration::from_millis(500);
		let peer = url("tcp://peer:1/");
		let (client, mut accepted) = flapping(INITIAL, MAX, TIMEOUT, vec![(peer.clone(), Flap::Goaway(String::new()))]);
		let connection = client.connect(peer);

		let mut accepts = Vec::new();
		for _ in 0..10 {
			accepts.push(accepted.recv().await.expect("the dialer dropped"));
		}
		assert!(
			accepts[9].1 - accepts[0].1 > TIMEOUT * 2,
			"the redials outran the backoff"
		);
		assert!(
			futures::FutureExt::now_or_never(connection.closed()).is_none(),
			"a restarting peer ended the connection"
		);
		assert_paced(&accepts, INITIAL, MAX);
	}

	/// Two peers that redirect to each other the moment a session is up are failed attempts:
	/// the delay escalates across both, and the timeout ends the loop.
	#[cfg(feature = "tcp")]
	#[tokio::test(start_paused = true)]
	async fn an_immediate_redirect_loop_escalates_until_the_timeout() {
		const TIMEOUT: Duration = Duration::from_secs(2);
		let a = url("tcp://peer:1/");
		let b = url("tcp://peer:2/");
		let peers = vec![
			(a.clone(), Flap::Goaway(b.to_string())),
			(b.clone(), Flap::Goaway(a.to_string())),
		];
		let (client, mut accepted) = flapping(INITIAL, MAX, TIMEOUT, peers);

		let started = moq_clock::now();
		let err = client
			.connect(a.clone())
			.closed()
			.await
			.expect_err("a redirect loop kept the connection");
		assert!(
			matches!(&err, Error::Reconnect(reason) if reason.contains("redirect")),
			"{err}"
		);
		assert_eq!(started.elapsed(), TIMEOUT, "the timeout did not end the loop");

		let mut accepts = Vec::new();
		while let Ok(accept) = accepted.try_recv() {
			accepts.push(accept);
		}
		assert!(accepts.len() >= 6, "only {} sessions", accepts.len());
		for (index, (peer, _)) in accepts.iter().enumerate() {
			assert_eq!(
				*peer,
				if index % 2 == 0 { a.clone() } else { b.clone() },
				"session {index}"
			);
		}
		assert_paced(&accepts, INITIAL, MAX);
	}

	/// A zero `initial` turns retry waiting off: every redial follows the close at once.
	#[cfg(feature = "tcp")]
	#[tokio::test(start_paused = true)]
	async fn a_zero_initial_redials_at_once() {
		const TIMEOUT: Duration = Duration::from_secs(1);
		let peer = url("tcp://peer:1/");
		let (client, mut accepted) = flapping(Duration::ZERO, MAX, TIMEOUT, vec![(peer.clone(), Flap::Close)]);
		let connection = client.connect(peer);

		let first = accepted.recv().await.expect("the dialer dropped").1;
		for _ in 0..10 {
			let at = accepted.recv().await.expect("the dialer dropped").1;
			assert_eq!(at, first, "a zero initial waited before a retry");
		}
		drop(connection);
	}

	#[test]
	fn redirect_loop_detection_catches_any_cycle_in_constant_memory() {
		for tail in 0..4 {
			for cycle in 2..=8 {
				// `tail` one-way moves, then `cycle` endpoints redirecting around a ring.
				let at = |hop: usize| {
					if hop < tail {
						url(&format!("tcp://tail{hop}/"))
					} else {
						url(&format!("tcp://ring{}/", (hop - tail) % cycle))
					}
				};
				let mut redirects = Redirects::default();
				let mut caught = None;
				for hop in 0..4 * (tail + cycle) {
					let looped = redirects.depart(at(hop), &at(hop + 1));
					match caught {
						Some(_) => assert!(looped, "a caught loop stays caught"),
						None if looped => caught = Some(hop),
						None => {}
					}
				}
				let caught = caught.unwrap_or_else(|| panic!("tail {tail}, cycle {cycle}: never caught"));
				// Never before the run first returns to an endpoint it left.
				assert!(
					caught + 1 >= tail + cycle,
					"tail {tail}, cycle {cycle}: caught at {caught}"
				);
			}
		}

		let mut redirects = Redirects::default();
		for hop in 0..1000 {
			let from = url(&format!("tcp://a{hop}/"));
			let to = url(&format!("tcp://a{}/", hop + 1));
			assert!(!redirects.depart(from, &to), "a one-way run never loops");
		}
	}

	#[test]
	fn an_endpoint_ignores_the_credentials() {
		assert_eq!(
			endpoint(&url("tcp://token-a:secret@relay:1/path?jwt=a#x")),
			endpoint(&url("tcp://token-b@relay:1/path?jwt=b"))
		);
		// The path is part of the request target, so it names a different endpoint.
		assert_ne!(endpoint(&url("tcp://relay:1/a")), endpoint(&url("tcp://relay:1/b")));
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
		|c: &Backoff| c.healthy(),
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
