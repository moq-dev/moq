use std::task::{Poll, ready};
use std::time::Duration;

use moq_net::Version;
use moq_net::bandwidth::{Consumer as BandwidthConsumer, Producer as BandwidthProducer};
use moq_net::kio;
use rand::RngExt;
use url::Url;

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
fn attempt_timeout(index: usize, total: usize) -> Option<Duration> {
	(index + 1 < total).then_some(CONNECT_ATTEMPT)
}

/// The retry window as a deadline for the address walk, or `None` to leave each
/// attempt to the client's own connect timeout.
///
/// Only when reconnecting. `Backoff::timeout` is how long to keep *retrying*, so
/// applying it to a one-shot dial would cut short the single attempt there will
/// ever be: a handshake slower than the retry window but well inside
/// [`crate::connect::Config::timeout`](crate::connect::Config::timeout) would fail for a
/// reason that does not apply. That deadline is the one governing a one-shot
/// dial, and it already does.
fn retry_budget(reconnect: bool, retry_start: tokio::time::Instant, timeout: Duration) -> Option<tokio::time::Instant> {
	(reconnect && !timeout.is_zero()).then(|| retry_start + timeout)
}

/// When the attempt at candidate `index` of `total` must be given up, or `None`
/// to let it run.
///
/// Two bounds, whichever comes first. [`attempt_timeout`] keeps one slow
/// candidate from eating the others' turn, and `budget` is the retry window from
/// [`Backoff::timeout`], which the walk has to respect too: bounding only the
/// non-final attempts left the last one to run to the client's connect timeout,
/// so a handful of black-holed addresses could blow through a give-up budget
/// several times over before reporting anything.
fn attempt_deadline(
	index: usize,
	total: usize,
	now: tokio::time::Instant,
	budget: Option<tokio::time::Instant>,
) -> Option<tokio::time::Instant> {
	// An equal share of what's left, not all of it. Handing each candidate the
	// whole remaining window lets the first couple spend it between them and
	// strand a reachable address further down the list, which is the opposite of
	// what walking the list is for. Recomputed per attempt, so one that fails fast
	// leaves more for the rest.
	let share = budget.map(|budget| {
		let remaining = total - index;
		now + budget.saturating_duration_since(now) / remaining as u32
	});
	let bound = attempt_timeout(index, total).map(|limit| now + limit);

	match (bound, share) {
		(Some(bound), Some(share)) => Some(bound.min(share)),
		(Some(only), None) | (None, Some(only)) => Some(only),
		(None, None) => None,
	}
}

/// Exponential backoff configuration for reconnection attempts.
///
/// Every field is optional so a value set in TOML survives the CLI re-parse that follows it; read
/// them through the accessors, which supply the defaults. The delays carry jitter, so a fleet
/// knocked offline together does not reconnect in lockstep. The timeout bounds every failure;
/// only a settled response from the server short-circuits it.
#[derive(Clone, Debug, Default, clap::Args, serde::Serialize, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
#[non_exhaustive]
pub struct Backoff {
	/// Initial delay before first reconnect attempt. Defaults to 1s.
	///
	/// Doubles as the bar a session must stay up to count as healthy, so it is
	/// floored at 50ms: at zero every session would look healthy and the retry
	/// pacing would collapse.
	#[serde(skip_serializing_if = "Option::is_none")]
	#[arg(
		id = "backoff-initial",
		long,
		env = "MOQ_BACKOFF_INITIAL",
		value_parser = humantime::parse_duration,
	)]
	#[serde(with = "humantime_serde")]
	pub initial: Option<Duration>,

	/// Multiplier applied to delay after each failure. Defaults to 2.
	#[serde(skip_serializing_if = "Option::is_none")]
	#[arg(id = "backoff-multiplier", long, env = "MOQ_BACKOFF_MULTIPLIER")]
	pub multiplier: Option<u32>,

	/// Maximum delay between reconnect attempts. Defaults to 5s.
	#[serde(skip_serializing_if = "Option::is_none")]
	#[arg(
		id = "backoff-max",
		long,
		env = "MOQ_BACKOFF_MAX",
		value_parser = humantime::parse_duration,
	)]
	#[serde(with = "humantime_serde")]
	pub max: Option<Duration>,

	/// Maximum time to spend retrying before giving up. Defaults to 10s.
	///
	/// Resets after a stable connection (one that outlives the initial backoff), so a flapping
	/// session that reconnects then immediately drops still counts toward the timeout. Set to 0 for
	/// unlimited retries.
	#[serde(skip_serializing_if = "Option::is_none")]
	#[arg(
		id = "backoff-timeout",
		long,
		env = "MOQ_BACKOFF_TIMEOUT",
		value_parser = humantime::parse_duration,
	)]
	#[serde(with = "humantime_serde")]
	pub timeout: Option<Duration>,
}

impl Backoff {
	/// The configured initial delay, or the default.
	pub fn initial(&self) -> Duration {
		self.initial.unwrap_or(DEFAULT_INITIAL)
	}

	/// The configured multiplier, or the default.
	pub fn multiplier(&self) -> u32 {
		self.multiplier.unwrap_or(DEFAULT_MULTIPLIER)
	}

	/// The configured delay ceiling, or the default.
	pub fn max(&self) -> Duration {
		self.max.unwrap_or(DEFAULT_MAX)
	}

	/// The configured give-up window, or the default. Zero retries forever.
	pub fn timeout(&self) -> Duration {
		self.timeout.unwrap_or(DEFAULT_TIMEOUT)
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
	/// The peer sent a GOAWAY; the replacement is being dialed while the old
	/// session keeps serving.
	Migrating,
}

/// What to do with the URI a peer names in its GOAWAY.
///
/// The URI is dialed exactly as given, so it must carry whatever credentials the
/// new endpoint needs. Nothing from the current session is copied onto it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, clap::ValueEnum, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
#[non_exhaustive]
pub enum Redirect {
	/// Follow any URI that neither downgrades the scheme nor widens what we can
	/// reach (a public endpoint redirecting to loopback, a private range, or IPC).
	#[default]
	Follow,
	/// Follow only when the host matches the configured URL, so a peer can move us
	/// between ports or schemes but not to another host.
	SameHost,
	/// Ignore the URI and redial the configured URL.
	Ignore,
}

impl Redirect {
	/// Resolve the URL to dial after a GOAWAY, falling back to `current` when the
	/// redirect is empty ("reconnect to me"), malformed, or refused by policy.
	pub fn resolve(&self, uri: &str, current: &Url) -> Url {
		if uri.is_empty() || matches!(self, Self::Ignore) {
			return current.clone();
		}

		let Ok(target) = uri.parse::<Url>() else {
			tracing::warn!(uri, "malformed GOAWAY URI; redialing the current URL");
			return current.clone();
		};

		if scheme_tier(target.scheme()) < scheme_tier(current.scheme()) {
			tracing::warn!(uri, "GOAWAY redirect downgrades the scheme; redialing the current URL");
			return current.clone();
		}

		// A peer must not be able to point us somewhere we could not already
		// reach: an authenticated upstream redirecting to a loopback or IPC
		// address turns a redirect into a probe of the local host.
		if is_local(&target) && !is_local(current) {
			tracing::warn!(uri, "GOAWAY redirect widens reachability; redialing the current URL");
			return current.clone();
		}

		// Host only, not the full authority: the port is what a peer legitimately
		// moves us across when it hands off to a sibling process on the same box.
		if matches!(self, Self::SameHost) && target.host_str() != current.host_str() {
			tracing::warn!(
				uri,
				"GOAWAY redirect leaves the current host; redialing the current URL"
			);
			return current.clone();
		}

		target
	}
}

/// Rank a scheme so a peer-supplied redirect cannot silently drop encryption.
/// Unknown schemes rank lowest, so a forgotten classification is refused.
fn scheme_tier(scheme: &str) -> u8 {
	match scheme {
		"https" | "moqt" | "moql" | "wss" | "iroh" => 2,
		"tcp" | "ws" | "http" => 1,
		// `unix` lands here deliberately: local IPC is not an upgrade over a
		// network transport, it is a different reachability class (see `is_local`).
		_ => 0,
	}
}

/// Whether a URL names something only reachable from this host or network.
fn is_local(url: &Url) -> bool {
	match url.host() {
		Some(url::Host::Domain(host)) => host == "localhost" || host.ends_with(".localhost"),
		Some(url::Host::Ipv4(ip)) => is_local_v4(ip),
		// An IPv4-mapped address reaches the same host as the v4 it wraps, so judge
		// it by that rather than by the v6 rules.
		Some(url::Host::Ipv6(ip)) => match ip.to_ipv4_mapped() {
			Some(v4) => is_local_v4(v4),
			// Loopback (::1), unspecified (::), unique local (fc00::/7), link local (fe80::/10).
			None => {
				ip.is_loopback()
					|| ip.is_unspecified()
					|| (ip.segments()[0] & 0xfe00) == 0xfc00
					|| (ip.segments()[0] & 0xffc0) == 0xfe80
			}
		},
		// No host at all, e.g. a `unix:` socket path.
		None => true,
	}
}

fn is_local_v4(ip: std::net::Ipv4Addr) -> bool {
	ip.is_loopback() || ip.is_private() || ip.is_link_local() || ip.is_unspecified()
}

/// How a reconnect loop reacts to a peer's GOAWAY.
#[derive(Clone, Debug, Default, clap::Args, serde::Serialize, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
#[non_exhaustive]
pub struct GoawayConfig {
	/// What to do with the URI a peer names in its GOAWAY. Defaults to
	/// [`Redirect::Follow`].
	#[arg(id = "goaway-redirect", long, env = "MOQ_GOAWAY_REDIRECT", value_enum)]
	pub redirect: Option<Redirect>,

	/// How long the old session keeps serving after its replacement connects, e.g.
	/// "10s" or "500ms". This is a cap: a GOAWAY naming a shorter deadline wins,
	/// since the peer force-closes at its own deadline regardless, but a longer one
	/// does not extend it. Defaults to 10 seconds.
	#[arg(
		id = "goaway-handover",
		long,
		env = "MOQ_GOAWAY_HANDOVER",
		value_parser = humantime::parse_duration,
	)]
	#[serde(default, with = "humantime_serde")]
	pub handover: Option<Duration>,
}

/// Defaults for the [`Backoff`] knobs, applied by its accessors when a field is unset.
const DEFAULT_INITIAL: Duration = Duration::from_secs(1);
const DEFAULT_MULTIPLIER: u32 = 2;
const DEFAULT_MAX: Duration = Duration::from_secs(5);
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(10);

/// Floor for the retry delay, which also sets the bar for calling a session
/// healthy. Small enough to stay out of the way of a fast config, large enough
/// that a session closed on sight never clears it.
const MIN_BACKOFF: Duration = Duration::from_millis(50);

/// The retry pacing a connection actually runs on: [`Backoff`] with the floors
/// applied.
///
/// Every knob can zero the delay on its own, and a delay of zero is a dial loop
/// that runs as fast as the runtime allows. `initial` seeds it, `multiplier`
/// grows it, and `max` caps it, so a zero anywhere collapses the whole schedule.
/// Resolving them once, here, keeps that from having to be re-argued at each of
/// the three places the loop reaches for them.
struct Pacing {
	/// The first delay, and the bar a session must clear to count as healthy. At
	/// zero every session looks healthy, which resets the give-up window forever.
	initial: Duration,
	/// Never below `initial`, so the cap cannot undo the floor on later retries.
	max: Duration,
	/// At least 1, since zero would shrink the delay back to nothing.
	multiplier: u32,
}

impl Pacing {
	fn new(backoff: &Backoff) -> Self {
		let initial = backoff.initial().max(MIN_BACKOFF);
		Self {
			initial,
			max: backoff.max().max(initial),
			multiplier: backoff.multiplier().max(1),
		}
	}

	/// The delay for the attempt after one that waited `delay`.
	fn next(&self, delay: Duration) -> Duration {
		std::cmp::min(delay.saturating_mul(self.multiplier), self.max)
	}
}

/// Default handover window, and the ceiling a GOAWAY deadline is capped to when
/// the config names none.
const DEFAULT_HANDOVER: Duration = Duration::from_secs(10);

impl GoawayConfig {
	/// The configured redirect policy, or the default.
	pub fn redirect(&self) -> Redirect {
		self.redirect.unwrap_or_default()
	}

	/// How long the old session keeps serving, given the deadline the peer's GOAWAY
	/// named (`None` when it named none).
	///
	/// Whichever comes first. The peer's deadline is a promise about when it
	/// force-closes, so waiting past it just holds a dead session; ours is the cap
	/// on how long we keep one around at all, so a peer naming an hour cannot talk
	/// us into honoring it.
	pub fn handover(&self, timeout: Option<Duration>) -> Duration {
		std::cmp::min(
			self.handover.unwrap_or(DEFAULT_HANDOVER),
			timeout.unwrap_or(Duration::MAX),
		)
	}
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
}

impl Shared {
	/// A session is live and serving.
	fn connected(&self, session: &moq_net::Session) {
		// Held across the publish: see [`CloseGuard`]. A close that already ran is
		// honored here rather than leaving this session parked in the final state.
		let closed = self.closed.lock().unwrap();
		if let Some(err) = closed.as_ref() {
			session.abort(err.clone());
			return;
		}
		if let Ok(mut state) = self.state.write() {
			state.status = Some(Status::Connected);
			state.version = Some(session.version());
			state.session = Some(session.clone());
		}
	}

	/// The peer sent a GOAWAY: the session in [`State`] keeps serving while its
	/// replacement is dialed, so only the status moves.
	fn migrating(&self) {
		if let Ok(mut state) = self.state.write() {
			state.status = Some(Status::Migrating);
		}
	}

	/// Nothing is live. Clears the session so a handle can't be handed a closed one,
	/// and drops the estimates that belonged to it.
	fn disconnected(&self) {
		if let Ok(mut state) = self.state.write() {
			state.status = Some(Status::Disconnected);
			state.version = None;
			state.session = None;
		}
		let _ = self.send_bw.set(None);
		let _ = self.recv_bw.set(None);
	}

	/// Record the error the loop gave up with, for [`Connection::closed`].
	fn failed(&self, err: Error) {
		if let Ok(mut state) = self.state.write() {
			state.error = Some(err);
		}
	}
}

impl Drop for Shared {
	fn drop(&mut self) {
		// The loop is over, including when its task was aborted out from under it.
		// Whoever still holds a consumer keeps the last state readable, so a
		// [`ConnectionStatsReader`] outliving every [`Connection`] would otherwise
		// keep the session clone parked here alive and the transport open with
		// nothing left able to reach it. Releasing it drops the last clone, which
		// is what closes the transport.
		self.disconnected();
	}
}

/// A cloneable read handle for the live connection stats of a [`Connection`].
///
/// Obtained via [`Connection::stats`]. [`stats`](Self::stats) returns `None` while the loop is
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

/// Handle to a connection maintained by a background task.
///
/// The task connects, waits for the session to end, then (unless reconnecting is
/// disabled, see [`crate::connect::Config::once`](crate::connect::Config::once))
/// redials with exponential backoff. The read surface mirrors [`moq_net::Session`]
/// so a caller can treat it like a session that transparently reconnects:
/// [`version`](Self::version), [`send_bandwidth`](Self::send_bandwidth),
/// and [`recv_bandwidth`](Self::recv_bandwidth) track the live session and reset while disconnected.
/// The extra toggle a plain session doesn't have is the connection lifecycle: [`established`](Self::established)
/// waits for the first session, [`connected`](Self::connected) reads the current state synchronously,
/// and [`status`](Self::status) waits for the next change. [`closed`](Self::closed)
/// waits for the loop to stop. Clones share the loop; it stops when the last
/// clone drops (or on an explicit [`close`](Self::close)).
#[derive(Clone)]
#[must_use = "dropping the Connection stops the dial; hold it for as long as you want the session"]
pub struct Connection {
	task: std::sync::Arc<AbortOnDrop>,
	state: kio::Consumer<State>,
	/// Persistent send-bitrate estimate, fed by the loop from each live session.
	send_bandwidth: BandwidthConsumer,
	/// Persistent recv-bitrate estimate, fed by the loop from each live session.
	recv_bandwidth: BandwidthConsumer,
	/// The last status returned by [`status`](Self::status), for change detection.
	/// Per-clone: a clone starts from its parent's cursor and diverges from there.
	last_reported: Option<Status>,
}

/// Aborts the connection loop when the last [`Connection`] clone drops.
struct AbortOnDrop {
	handle: tokio::task::AbortHandle,
	closed: CloseGuard,
}

impl Drop for AbortOnDrop {
	fn drop(&mut self) {
		self.handle.abort();
	}
}

/// Serializes [`Connection::abort`] against the loop publishing a fresh session.
///
/// Aborting a tokio task doesn't interrupt it before its next yield, so a redial
/// completing in that window would otherwise hand [`Shared::connected`] a session
/// that `abort` had already looked for and missed, and it would be closed by the
/// refcount drop instead of carrying the caller's error code to the peer. Both
/// sides take this lock around their `state` access, so the session is either
/// published before the close is recorded or refused after it.
type CloseGuard = std::sync::Arc<std::sync::Mutex<Option<moq_net::Error>>>;

impl Connection {
	pub(crate) fn new(client: Client, addrs: Addrs) -> Self {
		let producer = kio::Producer::<State>::default();
		let state = producer.consume();

		// The loop feeds these across every reconnect, so a consumer's handle survives session churn
		// (unlike a session's own bandwidth consumer, which dies with the session).
		let send_bw = BandwidthProducer::new();
		let recv_bw = BandwidthProducer::new();
		let send_bandwidth = send_bw.consume();
		let recv_bandwidth = recv_bw.consume();

		let closed: CloseGuard = Default::default();
		let task_closed = closed.clone();

		let task = tokio::spawn(async move {
			let reconnect = client.reconnect;
			let shared = Shared {
				state: producer,
				send_bw,
				recv_bw,
				closed: task_closed,
			};
			if let Err(err) = Self::run(&shared, client, addrs).await {
				// In one-shot mode the session ending is the expected lifecycle, and
				// its close reason arrives here; don't dress it up as a loop failure.
				match reconnect {
					true => tracing::error!(%err, "connection loop exited"),
					false => tracing::info!(%err, "connection closed"),
				}
				shared.failed(err);
			}
			// Dropping the producers here closes the channels, signaling consumers.
		});
		Self {
			task: std::sync::Arc::new(AbortOnDrop {
				handle: task.abort_handle(),
				closed,
			}),
			state,
			send_bandwidth,
			recv_bandwidth,
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

	/// Stop the loop now, for every clone. [`abort`](Self::abort) with no error code.
	///
	/// Prefer just dropping the last clone; this is for teardown paths that can't
	/// control which clone drops last.
	pub fn close(&self) {
		self.abort(moq_net::Error::Cancel);
	}

	async fn run(shared: &Shared, client: Client, addrs: Addrs) -> crate::Result<()> {
		let backoff = client.backoff.clone();
		let goaway = client.goaway.clone();
		let pacing = Pacing::new(&backoff);
		let initial = pacing.initial;
		let mut delay = initial;
		let mut retry_start = tokio::time::Instant::now();
		let mut last_error: Option<Error> = None;
		// Sticky across migrations: a redirect is an assignment, not a detour, so a
		// later drop redials wherever we were last sent. Scoped to this loop, so a
		// fresh Connection starts from the configured addresses again.
		let mut addrs = addrs;
		// An old session kept alive after a GOAWAY so its in-flight groups finish.
		let mut draining: Option<Draining> = None;

		loop {
			let timeout = backoff.timeout();
			if !timeout.is_zero() && retry_start.elapsed() >= timeout {
				return Err(timeout_error(timeout, last_error.as_ref()));
			}

			let budget = retry_budget(client.reconnect, retry_start, timeout);

			match Self::dial_any(shared, &client, &addrs, &mut draining, budget).await {
				Ok((url, session)) => {
					tracing::info!(peer = %Endpoint(&url), "connected");
					shared.connected(&session);

					let connected = tokio::time::Instant::now();
					// Wait for the session to end, forwarding its bandwidth estimates into the
					// persistent producers meanwhile so consumers track the live stats across the
					// connection, and draining any predecessor left over from a migration.
					let ended = run_session(shared, &session, &mut draining).await;

					// A session that stayed up past the initial backoff is healthy; one that
					// ended sooner counts as a failed attempt however it ended.
					let healthy = connected.elapsed() >= initial;

					// One-shot mode leaves rather than migrating: there is no replacement to
					// dial, and a GOAWAY naming no deadline never force-closes, so the peer
					// stops accepting requests and waits for us. Ignoring it is how both
					// sides end up waiting on each other forever. Checked before the
					// migration branch below, which would otherwise loop back to redial.
					if !client.reconnect && matches!(ended, Ended::Goaway(_)) {
						shared.disconnected();
						return Ok(());
					}

					if let Ended::Goaway(msg) = &ended {
						// A redirect is an assignment: keep dialing it from here on, and
						// only it. The peer named exactly one place to go, which retires
						// whatever other addresses got us to this session.
						let url = goaway.redirect().resolve(&msg.uri, &url);
						addrs = Addrs::new(url.clone());

						// Hand over gracefully however the backoff bookkeeping scores this
						// session. The old one keeps serving until it closes or overstays,
						// so its routes stay attached and live tracks splice onto the
						// replacement at a group boundary. Tearing it down here instead
						// would drop every group published until the replacement caught up.
						tracing::info!(peer = %Endpoint(&url), "upstream GOAWAY; migrating");
						shared.migrating();
						// Retire any predecessor first: overwriting would drop its deadline
						// on the floor and leave it holding the connection open.
						if let Some(mut old) = draining.take() {
							old.retire();
						}
						draining = Some(Draining::new(session, goaway.handover(msg.timeout)));

						if healthy {
							delay = initial;
							retry_start = tokio::time::Instant::now();
							last_error = None;
							// No backoff sleep: a handover off a healthy session is not a failure.
							continue;
						}

						// Redirected almost immediately. Still follow it, but score it as a
						// failed attempt so two peers bouncing us between them escalate
						// through backoff and eventually give up. The old session serves
						// across the sleep, so the redirect loop costs time, not data.
						last_error = Some(Error::Reconnect("peer redirected immediately".to_string()));
						let Some(wait) = retry_wait(delay, retry_start, timeout) else {
							return Err(timeout_error(timeout, last_error.as_ref()));
						};
						tracing::warn!(peer = %Endpoint(&url), ?wait, "peer redirected immediately; retrying after backoff");
						// Keep the handover bounded across the sleep: nothing else polls the
						// predecessor while the loop is between connections.
						sleep_draining(wait, &mut draining, shared).await;
						delay = pacing.next(delay);
						continue;
					}

					shared.disconnected();

					// An auth rejection is terminal however long the session lived, and
					// whether or not we would otherwise redial: the wire's UNAUTHORIZED is
					// specified, so this is the peer telling us these credentials will
					// never work, not a code we guessed at.
					if let Ended::Closed(Err(err)) = &ended {
						let err = Error::from(err.clone());
						if err.is_auth() {
							return Err(err);
						}
					}

					// One-shot mode: the session ending ends the connection. Its close
					// reason is the terminal error, mirroring `moq_net::Session::closed`.
					// A GOAWAY already returned above.
					if !client.reconnect {
						return match ended {
							Ended::Closed(res) => res.map_err(Error::from),
							Ended::Goaway(_) => unreachable!("handled before the migration branch"),
						};
					}

					if healthy {
						// Reset the backoff window so a one-off drop reconnects promptly.
						tracing::warn!(peer = %Endpoint(&url), "session closed, reconnecting");
						delay = initial;
						retry_start = tokio::time::Instant::now();
						last_error = None;
					} else {
						// Connected then dropped almost immediately (e.g. the server accepts then
						// resets, or redirects us straight back out). Treat it as a failed
						// connection: keep the reason so the give-up timeout reports a real cause,
						// and fall through to the shared backoff sleep below so repeated flaps
						// escalate instead of spinning the CPU. This is what bounds a redirect
						// loop between two peers.
						let err = match ended {
							Ended::Closed(Err(err)) => Some(Error::from(err)),
							Ended::Closed(Ok(())) => None,
							// Handled above: a GOAWAY never reaches here.
							Ended::Goaway(_) => None,
						};
						// NOTE: only UNAUTHORIZED is specified, and it is handled above. Any
						// other MoQ-layer rejection (Request::close after the transport is
						// accepted) lands here as an untyped transport close, so it cannot be
						// told apart from a network blip and is retried until the give-up
						// timeout. Classifying the rest needs the transport to surface the
						// close code; until then, one-shot mode (`reconnect = false`) is how
						// a caller observes those rejections directly.
						match err {
							Some(err) => {
								tracing::warn!(peer = %Endpoint(&url), %err, "session severed immediately, retrying");
								last_error = Some(err);
							}
							None => tracing::warn!(peer = %Endpoint(&url), "session severed immediately, retrying"),
						}
					}
				}
				Err(err) => {
					if err.is_auth()
						|| err
							.status()
							.is_some_and(|status| !crate::error::status_retryable(status))
						|| !client.reconnect
					{
						return Err(err);
					}
					last_error = Some(err);
				}
			}

			let Some(wait) = retry_wait(delay, retry_start, timeout) else {
				return Err(timeout_error(timeout, last_error.as_ref()));
			};
			// No URL here: with several candidates there isn't one to name, and each
			// attempt already logged the address it tried.
			tracing::warn!(?wait, "reconnecting after backoff");
			// Drain-aware: a GOAWAY off a healthy session continues straight to the
			// replacement dial, so a predecessor can still be draining when that dial
			// fails and lands here. A plain sleep would stop enforcing its handover
			// until some later dial succeeded, holding an upstream that asked to drain
			// open for the whole retry window, or forever with `--backoff-timeout=0`.
			sleep_draining(wait, &mut draining, shared).await;
			delay = pacing.next(delay);
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
		budget: Option<tokio::time::Instant>,
	) -> crate::Result<(Url, moq_net::Session)> {
		let candidates = addrs.as_slice();
		let mut last = None;

		for (index, url) in candidates.iter().enumerate() {
			// The retry window can run out mid-walk. Stop rather than starting an
			// attempt with no time to finish; the caller reports the budget error.
			if budget.is_some_and(|budget| tokio::time::Instant::now() >= budget) {
				break;
			}

			tracing::info!(peer = %Endpoint(url), "connecting");

			let mut dial = std::pin::pin!(client.dial(url.clone()));
			let dialed = kio::wait(|waiter| {
				if poll_draining(draining, waiter) {
					shared.disconnected();
				}
				waiter.poll_future(dial.as_mut())
			});

			let deadline = attempt_deadline(index, candidates.len(), tokio::time::Instant::now(), budget);
			let dialed = match deadline {
				None => dialed.await,
				Some(deadline) => match tokio::time::timeout_at(deadline, dialed).await {
					Ok(dialed) => dialed,
					Err(_) => Err(Error::Reconnect(format!("timed out connecting to {}", Endpoint(url)))),
				},
			};

			match dialed {
				Ok(session) => return Ok((url.clone(), session)),
				// A status the peer actually sent is its answer, not this address's, so
				// unless it invites another attempt it settles the whole walk. Carrying
				// on would offer the same rejected credentials at the peer's other
				// addresses, and worse, a later transport failure would overwrite the
				// real reason and leave the outer loop retrying until its budget ran out.
				Err(err)
					if err
						.status()
						.is_some_and(|status| !crate::error::status_retryable(status)) =>
				{
					return Err(err);
				}
				Err(err) => {
					tracing::debug!(peer = %Endpoint(url), %err, "address unreachable");
					last = Some(err);
				}
			}
		}

		// `Addrs` is never empty, so the only way to get here without a failure is
		// the budget running out before the first attempt started.
		Err(last.unwrap_or_else(|| Error::Reconnect("retry window elapsed before dialing".to_string())))
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

	/// The negotiated MoQ version of the live session, or `None` while disconnected.
	///
	/// The [`moq_net::Session::version`] counterpart; `Option` because a reconnecting handle can be
	/// between sessions.
	pub fn version(&self) -> Option<Version> {
		self.state.read().version
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

	/// A cloneable handle for reading the current connection's stats.
	///
	/// The handle keeps working across reconnects, reporting `None` between connections.
	pub fn stats(&self) -> ConnectionStatsReader {
		ConnectionStatsReader {
			state: self.state.clone(),
		}
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
fn retry_wait(delay: Duration, retry_start: tokio::time::Instant, timeout: Duration) -> Option<Duration> {
	let wait = delay.mul_f64(0.5 + rand::rng().random::<f64>() / 2.0);
	if timeout.is_zero() {
		return Some(wait);
	}
	Some(wait.min(timeout.checked_sub(retry_start.elapsed())?))
}

/// Why a session stopped being the live one.
enum Ended {
	/// The transport closed.
	Closed(Result<(), moq_net::Error>),
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
	deadline: std::pin::Pin<Box<tokio::time::Sleep>>,
}

impl Draining {
	fn new(session: moq_net::Session, handover: Duration) -> Self {
		let closed = {
			let session = session.clone();
			Box::pin(async move {
				session.closed().await;
			}) as std::pin::Pin<Box<dyn Future<Output = ()> + Send>>
		};

		Self {
			session,
			closed,
			deadline: Box::pin(tokio::time::sleep(handover)),
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
	let mut sleep = std::pin::pin!(tokio::time::sleep(delay));
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
/// polled through the waiter's own waker. `migrate` is whether a GOAWAY ends this session's
/// tenure ([`Ended::Goaway`]); when false the arm isn't polled and only a close returns.
async fn run_session(shared: &Shared, session: &moq_net::Session, draining: &mut Option<Draining>) -> Ended {
	let mut send = session.send_bandwidth();
	let mut recv = session.recv_bandwidth();
	let goaway = session.draining();
	let closed = session.closed();
	tokio::pin!(closed);

	kio::wait(|waiter| {
		poll_forward(&mut send, &shared.send_bw, waiter);
		poll_forward(&mut recv, &shared.recv_bw, waiter);

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
		if let Poll::Ready(Ok(msg)) = goaway.poll(waiter) {
			return Poll::Ready(Ended::Goaway(msg));
		}

		waiter.poll_future(closed.as_mut()).map(|err| Ended::Closed(Err(err)))
	})
	.await
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

	/// The same clobber guard for [`Backoff`]. A bare field with a `default_value`
	/// would be rewritten by the CLI re-parse that follows a TOML load, silently
	/// resetting retry tuning that was only ever configured in the file.
	#[test]
	fn cli_does_not_clobber_toml_backoff() {
		use clap::Parser;

		#[derive(Parser)]
		struct Wrapper {
			#[command(flatten)]
			backoff: Backoff,
		}

		// Stand in for the TOML layer, then re-apply the CLI with none of the flags set.
		let mut parsed = Wrapper::parse_from(["test"]);
		parsed.backoff.initial = Some(Duration::from_secs(7));
		parsed.backoff.multiplier = Some(5);
		parsed.backoff.max = Some(Duration::from_secs(11));
		parsed.backoff.timeout = Some(Duration::ZERO);
		parsed.update_from(["test"]);

		assert_eq!(parsed.backoff.initial, Some(Duration::from_secs(7)));
		assert_eq!(parsed.backoff.multiplier, Some(5));
		assert_eq!(parsed.backoff.max, Some(Duration::from_secs(11)));
		assert_eq!(parsed.backoff.timeout, Some(Duration::ZERO), "0 means retry forever");

		// Unset stays unset, so the accessors supply the documented defaults.
		let parsed = Wrapper::parse_from(["test"]);
		assert_eq!(parsed.backoff.initial, None);
		assert_eq!(parsed.backoff.initial(), DEFAULT_INITIAL);
		assert_eq!(parsed.backoff.multiplier(), DEFAULT_MULTIPLIER);
		assert_eq!(parsed.backoff.max(), DEFAULT_MAX);
		assert_eq!(parsed.backoff.timeout(), DEFAULT_TIMEOUT);

		// And a flag still lands where the merge can see it.
		let parsed = Wrapper::parse_from(["test", "--backoff-initial", "3s"]);
		assert_eq!(parsed.backoff.initial, Some(Duration::from_secs(3)));
	}

	/// The clap+TOML clobber guard: both `GoawayConfig` fields are `Option<T>` with
	/// no `default_value`, so a TOML-configured value survives the CLI re-parse
	/// that follows. A bare scalar with a clap default would silently win instead.
	#[test]
	fn cli_does_not_clobber_toml_goaway() {
		use clap::Parser;

		#[derive(Parser)]
		struct Wrapper {
			#[command(flatten)]
			goaway: GoawayConfig,
		}

		// No flags passed: the fields stay None so a TOML layer underneath keeps its
		// values, and the accessors supply the documented defaults.
		let parsed = Wrapper::parse_from(["test"]);
		assert_eq!(parsed.goaway.redirect, None);
		assert_eq!(parsed.goaway.handover, None);
		assert_eq!(parsed.goaway.redirect(), Redirect::Follow);
		assert_eq!(parsed.goaway.handover(None), Duration::from_secs(10));

		// Flags passed: they land where the merge can see them.
		let parsed = Wrapper::parse_from(["test", "--goaway-redirect", "ignore", "--goaway-handover", "3s"]);
		assert_eq!(parsed.goaway.redirect, Some(Redirect::Ignore));
		assert_eq!(parsed.goaway.handover, Some(Duration::from_secs(3)));
	}

	/// Our own window caps the peer's. A GOAWAY deadline shortens the handover but
	/// never extends it, so an upstream naming an absurd one cannot make us hold a
	/// dying session open for it.
	#[test]
	fn handover_takes_the_earlier_deadline() {
		let config = GoawayConfig {
			handover: Some(Duration::from_secs(10)),
			..Default::default()
		};

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
		let default = GoawayConfig::default();
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
		assert_eq!(Redirect::Follow.resolve("https://[::ffff:127.0.0.1]/", &public), public);
		assert_eq!(
			Redirect::Follow.resolve("https://127.0.0.1:9999/", &localhost).port(),
			Some(9999)
		);
	}

	/// The scheme ranking is what stops a peer from quietly moving us off an
	/// encrypted transport. An unknown scheme ranks lowest so a classification we
	/// forgot to add is refused rather than trusted.
	#[test]
	fn scheme_tiers_rank_encrypted_above_plaintext() {
		for scheme in ["https", "moqt", "moql", "wss", "iroh"] {
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

	/// A redirect may hold the scheme or improve it, never weaken it.
	#[test]
	fn resolve_refuses_a_scheme_downgrade() {
		let secure: Url = "https://relay.example/".parse().unwrap();
		let plain: Url = "http://relay.example/".parse().unwrap();

		// Downgrades fall back to the current URL rather than being followed.
		assert_eq!(Redirect::Follow.resolve("http://other.example/", &secure), secure);
		assert_eq!(Redirect::Follow.resolve("unix:///tmp/moq.sock", &secure), secure);

		// Same tier and upgrades are followed.
		let same: Url = "https://other.example/".parse().unwrap();
		assert_eq!(Redirect::Follow.resolve("https://other.example/", &secure), same);
		assert_eq!(Redirect::Follow.resolve("https://other.example/", &plain), same);
	}

	/// The three ways a redirect resolves to "redial what we already had": the peer
	/// naming no URI, a URI we cannot parse, and a policy that ignores it outright.
	#[test]
	fn resolve_falls_back_to_the_current_url() {
		let current: Url = "https://relay.example/".parse().unwrap();

		assert_eq!(
			Redirect::Follow.resolve("", &current),
			current,
			"empty means 'reconnect to me'"
		);
		assert_eq!(
			Redirect::Follow.resolve("not a url", &current),
			current,
			"a malformed URI is not a reason to stop reconnecting"
		);
		assert_eq!(
			Redirect::Ignore.resolve("https://other.example/", &current),
			current,
			"Ignore never leaves the configured URL"
		);
	}

	/// `SameHost` lets a peer move us between ports or schemes on the endpoint we
	/// already chose, but not onto a different host.
	#[test]
	fn resolve_same_host_pins_the_authority() {
		let current: Url = "https://relay.example:4443/".parse().unwrap();

		assert_eq!(
			Redirect::SameHost.resolve("https://elsewhere.example/", &current),
			current,
			"another host is refused"
		);

		let moved = Redirect::SameHost.resolve("https://relay.example:5443/", &current);
		assert_eq!(
			moved.port(),
			Some(5443),
			"a different port on the same host is followed"
		);
	}

	/// Every pacing knob can zero the retry delay on its own, and a zero delay is a
	/// dial loop bounded by nothing. The floors are what keep a degenerate config
	/// (or a binding that passes zeros) from turning unlimited retries into a spin.
	#[test]
	fn pacing_floors_every_degenerate_knob() {
		// A zero initial would also make every session look healthy, resetting the
		// give-up window forever.
		let pacing = Pacing::new(&Backoff {
			initial: Some(Duration::ZERO),
			..Default::default()
		});
		assert_eq!(pacing.initial, MIN_BACKOFF);
		assert!(pacing.next(pacing.initial) >= MIN_BACKOFF);

		// A cap below the floor would clamp the delay straight back down.
		let pacing = Pacing::new(&Backoff {
			max: Some(Duration::ZERO),
			..Default::default()
		});
		assert_eq!(pacing.max, pacing.initial);
		assert_eq!(pacing.next(pacing.initial), pacing.initial);

		// A zero multiplier would shrink it to nothing on the second attempt.
		let pacing = Pacing::new(&Backoff {
			multiplier: Some(0),
			..Default::default()
		});
		assert_eq!(pacing.next(pacing.initial), pacing.initial);

		// All at once: still paced.
		let pacing = Pacing::new(&Backoff {
			initial: Some(Duration::ZERO),
			max: Some(Duration::ZERO),
			multiplier: Some(0),
			..Default::default()
		});
		let mut delay = pacing.initial;
		for _ in 0..10 {
			delay = pacing.next(delay);
			assert!(delay >= MIN_BACKOFF, "the retry delay collapsed to {delay:?}");
		}
	}

	/// A sane config is left alone: the delay doubles up to the cap and stays there.
	#[test]
	fn pacing_grows_to_the_cap() {
		let pacing = Pacing::new(&Backoff {
			initial: Some(Duration::from_millis(100)),
			multiplier: Some(2),
			max: Some(Duration::from_millis(400)),
			..Default::default()
		});

		assert_eq!(pacing.initial, Duration::from_millis(100));
		assert_eq!(pacing.next(Duration::from_millis(100)), Duration::from_millis(200));
		assert_eq!(pacing.next(Duration::from_millis(200)), Duration::from_millis(400));
		assert_eq!(pacing.next(Duration::from_millis(400)), Duration::from_millis(400));
	}

	#[test]
	fn test_backoff_default() {
		let backoff = Backoff::default();
		assert_eq!(backoff.initial(), Duration::from_secs(1));
		assert_eq!(backoff.multiplier(), 2);
		assert_eq!(backoff.max(), Duration::from_secs(5));
		assert_eq!(backoff.timeout(), Duration::from_secs(10));
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

	const WALK_TIMEOUT: Duration = Duration::from_secs(30);

	fn url(value: &str) -> Url {
		value.parse().expect("valid url")
	}

	/// A loopback TCP port with nothing on it, which refuses instantly.
	///
	/// A dead address that *refuses* rather than black-holes is what keeps these
	/// tests fast and deterministic: the walk itself is what's under test, and
	/// bounding a black-holed attempt is covered by
	/// [`only_a_candidate_with_a_fallback_is_bounded`] as a pure function.
	#[cfg(feature = "tcp")]
	fn refused() -> Url {
		let probe = std::net::TcpListener::bind("127.0.0.1:0").expect("bind probe");
		let port = probe.local_addr().expect("local addr").port();
		drop(probe);
		url(&format!("tcp://127.0.0.1:{port}/"))
	}

	/// A bound stream listener, the URL that reaches it, and a client for it.
	#[cfg(feature = "tcp")]
	fn pair() -> (crate::Server, Url, Client) {
		let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();

		for _ in 0..20 {
			let probe = std::net::TcpListener::bind("127.0.0.1:0").expect("bind probe");
			let port = probe.local_addr().expect("local addr").port();
			drop(probe);

			let mut config = crate::listen::Config::default();
			config.tcp.bind = Some(format!("127.0.0.1:{port}").parse().expect("valid address"));
			let Ok(server) = config.init(Default::default()) else {
				continue;
			};

			let mut config = crate::connect::Config::default();
			config.tls.insecure = Some(true);
			let client = config.init(Default::default()).expect("build client");

			return (server, url(&format!("tcp://127.0.0.1:{port}/")), client);
		}
		panic!("could not bind a free TCP port after 20 attempts");
	}

	fn shared() -> Shared {
		Shared {
			state: kio::Producer::default(),
			send_bw: BandwidthProducer::new(),
			recv_bw: BandwidthProducer::new(),
			closed: CloseGuard::default(),
		}
	}

	/// The first address that answers wins, however many dead ones precede it.
	/// This is what keeps a peer reachable when discovery advertises an interface
	/// that doesn't route from here ahead of the one that does.
	#[cfg(feature = "tcp")]
	#[tokio::test]
	async fn dial_any_walks_past_the_dead_addresses() {
		let (server, live, client) = pair();
		let mut server = server.listen().await.expect("listen");
		tokio::spawn(async move { while server.accept().await.is_some() {} });

		let addrs = Addrs::collect([refused(), refused(), live.clone()]).expect("not empty");
		let shared = shared();
		let mut draining = None;

		let (connected, _session) = tokio::time::timeout(
			WALK_TIMEOUT,
			Connection::dial_any(&shared, &client, &addrs, &mut draining, None),
		)
		.await
		.expect("the walk must not hang")
		.expect("the live address must connect");
		assert_eq!(connected, live, "the walk must land on the one that answers");
	}

	/// With nothing reachable the walk reports a failure rather than hanging.
	#[cfg(feature = "tcp")]
	#[tokio::test]
	async fn dial_any_reports_failure_when_nothing_answers() {
		let (_server, _live, client) = pair();

		let addrs = Addrs::collect([refused(), refused()]).expect("not empty");
		let shared = shared();
		let mut draining = None;

		let result = tokio::time::timeout(
			WALK_TIMEOUT,
			Connection::dial_any(&shared, &client, &addrs, &mut draining, None),
		)
		.await
		.expect("the walk must not hang");
		assert!(result.is_err(), "every address refused, so the walk must fail");
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

		let (_server, _live, client) = pair();
		let mut target = refused();
		target.set_path(&format!("/.cluster/{SECRET}"));

		let addrs = Addrs::new(target);
		let shared = shared();
		let mut draining = None;
		// `Session` isn't `Debug`, so unwrap the failure by hand.
		let Err(err) = Connection::dial_any(&shared, &client, &addrs, &mut draining, None).await else {
			panic!("a refused address must fail");
		};

		// The log line did happen, so the assertion below isn't vacuously true.
		assert!(logs_contain("connecting"), "the dial never logged at all");
		assert!(!logs_contain(SECRET), "a log line leaked the credential");
		assert!(!format!("{err}").contains(SECRET), "the error leaked it: {err}");
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

	/// The retry window bounds the walk only when there are retries.
	///
	/// It is a budget for *retrying*, so spending it on the single attempt a
	/// one-shot dial gets would fail a handshake that is merely slower than the
	/// retry window while well inside the connect timeout that actually governs it.
	#[test]
	fn only_a_reconnecting_dial_spends_the_retry_window() {
		let start = tokio::time::Instant::now();
		let timeout = Duration::from_secs(10);

		assert_eq!(retry_budget(true, start, timeout), Some(start + timeout));
		assert_eq!(
			retry_budget(false, start, timeout),
			None,
			"a one-shot dial is bounded by the connect timeout, not the retry window"
		);
		assert_eq!(
			retry_budget(true, start, Duration::ZERO),
			None,
			"zero means retry forever, so nothing bounds the walk"
		);
	}

	/// Whichever of the two bounds comes first wins, and the retry window applies
	/// to every candidate including the last.
	#[test]
	fn the_retry_window_bounds_the_walk_too() {
		let now = tokio::time::Instant::now();

		// No budget: only the fixed per-candidate bound applies, and the last
		// candidate runs free.
		assert_eq!(attempt_deadline(0, 2, now, None), Some(now + CONNECT_ATTEMPT));
		assert_eq!(attempt_deadline(1, 2, now, None), None);

		// A tight budget is split, not spent by whoever asks first. Two seconds
		// across two candidates is a second each, so the second one still gets a
		// turn; handing the first the whole window would strand it.
		let tight = now + Duration::from_secs(2);
		assert_eq!(
			attempt_deadline(0, 2, now, Some(tight)),
			Some(now + Duration::from_secs(1))
		);
		// The last candidate gets what is actually left, which here is all of it
		// because nothing has been spent yet in this synthetic call.
		assert_eq!(attempt_deadline(1, 2, now, Some(tight)), Some(tight));

		// The share is what a black-holed pair used to exhaust: with the default
		// 10s window and the 5s bound, two candidates ate it and a reachable third
		// was never dialed. Now each gets a third.
		let default_window = now + Duration::from_secs(10);
		assert_eq!(
			attempt_deadline(0, 3, now, Some(default_window)),
			Some(now + Duration::from_secs(10) / 3),
			"a reachable third candidate must still get a turn"
		);

		// A budget with room to spare leaves the per-candidate bound in charge, so
		// one slow address still cannot eat the others' turn.
		let loose = now + Duration::from_secs(60);
		assert_eq!(attempt_deadline(0, 2, now, Some(loose)), Some(now + CONNECT_ATTEMPT));
		assert_eq!(attempt_deadline(1, 2, now, Some(loose)), Some(loose));
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
}
