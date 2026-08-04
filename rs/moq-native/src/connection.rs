use std::task::{Poll, ready};
use std::time::Duration;

use moq_net::Version;
use moq_net::bandwidth::{Consumer as BandwidthConsumer, Producer as BandwidthProducer};
use moq_net::kio;
use url::Url;

use crate::{Client, Error};

/// Exponential backoff configuration for reconnection attempts.
///
/// Every field is optional so a value set in TOML survives the CLI re-parse that
/// follows it; read them through the accessors, which supply the defaults.
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

	/// Maximum delay between reconnect attempts. Defaults to 30s.
	#[serde(skip_serializing_if = "Option::is_none")]
	#[arg(
		id = "backoff-max",
		long,
		env = "MOQ_BACKOFF_MAX",
		value_parser = humantime::parse_duration,
	)]
	#[serde(with = "humantime_serde")]
	pub max: Option<Duration>,

	/// Maximum time to spend retrying before giving up. Defaults to 5m.
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

	/// How long broadcasts fed by a reconnecting session should outlive a session
	/// drop (see [`moq_net::origin::Info::linger`]): slightly past the give-up
	/// [`timeout`](Self::timeout), so when the loop does give up its error surfaces
	/// before the broadcasts tear down. A zero timeout retries forever, so the
	/// broadcasts linger forever too.
	pub fn linger(&self) -> Duration {
		let timeout = self.timeout();
		match timeout.is_zero() {
			true => Duration::MAX,
			false => timeout.saturating_add(Duration::from_secs(1)),
		}
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
const DEFAULT_MAX: Duration = Duration::from_secs(30);
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(300);

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
}

impl Shared {
	/// A session is live and serving.
	fn connected(&self, session: &moq_net::Session) {
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
		// nothing left able to reach it. Releasing it here is not a close: a caller
		// holding a session of their own keeps it, which is the whole point of
		// [`Connection::session`].
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
/// disabled, see [`ClientConfig::reconnect`](crate::ClientConfig::reconnect))
/// redials with exponential backoff. The read surface mirrors [`moq_net::Session`]
/// so a caller can treat it like a session that transparently reconnects:
/// [`version`](Self::version), [`send_bandwidth`](Self::send_bandwidth),
/// and [`recv_bandwidth`](Self::recv_bandwidth) track the live session and reset while disconnected.
/// The extra toggle a plain session doesn't have is the connection lifecycle: [`established`](Self::established)
/// waits for the first session, [`connected`](Self::connected) reads the current state synchronously,
/// and [`status`](Self::status) waits for the next change. [`closed`](Self::closed)
/// waits for the loop to stop. Dropping the handle aborts the background task.
pub struct Connection {
	abort: tokio::task::AbortHandle,
	state: kio::Consumer<State>,
	/// Persistent send-bitrate estimate, fed by the loop from each live session.
	send_bandwidth: BandwidthConsumer,
	/// Persistent recv-bitrate estimate, fed by the loop from each live session.
	recv_bandwidth: BandwidthConsumer,
	/// The last status returned by [`status`](Self::status), for change detection.
	last_reported: Option<Status>,
}

impl Connection {
	pub(crate) fn new(client: Client, url: Url) -> Self {
		let producer = kio::Producer::<State>::default();
		let state = producer.consume();

		// The loop feeds these across every reconnect, so a consumer's handle survives session churn
		// (unlike a session's own bandwidth consumer, which dies with the session).
		let send_bw = BandwidthProducer::new();
		let recv_bw = BandwidthProducer::new();
		let send_bandwidth = send_bw.consume();
		let recv_bandwidth = recv_bw.consume();

		let task = tokio::spawn(async move {
			let reconnect = client.reconnect;
			let shared = Shared {
				state: producer,
				send_bw,
				recv_bw,
			};
			if let Err(err) = Self::run(&shared, client, url).await {
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
			abort: task.abort_handle(),
			state,
			send_bandwidth,
			recv_bandwidth,
			last_reported: None,
		}
	}

	async fn run(shared: &Shared, client: Client, url: Url) -> crate::Result<()> {
		let backoff = client.backoff.clone();
		let goaway = client.goaway.clone();
		let pacing = Pacing::new(&backoff);
		let initial = pacing.initial;
		let mut delay = initial;
		let mut retry_start = tokio::time::Instant::now();
		let mut last_error: Option<Error> = None;
		// Sticky across migrations: a redirect is an assignment, not a detour, so a
		// later drop redials wherever we were last sent. Scoped to this loop, so a
		// fresh Connection starts from the configured URL again.
		let mut url = url;
		// An old session kept alive after a GOAWAY so its in-flight groups finish.
		let mut draining: Option<Draining> = None;

		loop {
			let timeout = backoff.timeout();
			if !timeout.is_zero() && retry_start.elapsed() > timeout {
				let msg = match last_error {
					Some(err) => format!("reconnect timed out after {timeout:?}: {err}"),
					None => format!("reconnect timed out after {timeout:?}"),
				};
				return Err(Error::Reconnect(msg));
			}

			tracing::info!(%url, "connecting");

			match client.dial(url.clone()).await {
				Ok(session) => {
					tracing::info!(%url, "connected");
					shared.connected(&session);

					let connected = tokio::time::Instant::now();
					// Wait for the session to end, forwarding its bandwidth estimates into the
					// persistent producers meanwhile so consumers track the live stats across the
					// connection, and draining any predecessor left over from a migration.
					// One-shot mode ignores GOAWAY: it cannot dial the replacement, and the
					// peer force-closes at its own deadline anyway.
					let ended = run_session(shared, &session, &mut draining, client.reconnect).await;

					// A session that stayed up past the initial backoff is healthy; one that
					// ended sooner counts as a failed attempt however it ended.
					let healthy = connected.elapsed() >= initial;

					if let Ended::Goaway(msg) = &ended {
						// A redirect is an assignment: keep dialing it from here on.
						url = goaway.redirect().resolve(&msg.uri, &url);

						// Hand over gracefully however the backoff bookkeeping scores this
						// session. The old one keeps serving until it closes or overstays,
						// so its routes stay attached and live tracks splice onto the
						// replacement at a group boundary. Tearing it down here instead
						// would drop every group published until the replacement caught up.
						tracing::info!(%url, "upstream GOAWAY; migrating");
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
						tracing::warn!(%url, ?delay, "peer redirected immediately; retrying after backoff");
						// Keep the handover bounded across the sleep: nothing else polls the
						// predecessor while the loop is between connections.
						sleep_draining(delay, &mut draining, shared).await;
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
					if !client.reconnect {
						return match ended {
							Ended::Closed(res) => res.map_err(Error::from),
							// The GOAWAY arm is not polled in one-shot mode.
							Ended::Goaway(_) => Ok(()),
						};
					}


					if healthy {
						// Reset the backoff window so a one-off drop reconnects promptly.
						tracing::warn!(%url, "session closed, reconnecting");
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
						match err {
							Some(err) => {
								tracing::warn!(%url, %err, "session severed immediately, retrying");
								last_error = Some(err);
							}
							None => tracing::warn!(%url, "session severed immediately, retrying"),
						}
					}
				}
				Err(err) => {
					if err.is_auth() || !client.reconnect {
						return Err(err);
					}
					last_error = Some(err);
				}
			}

			tracing::warn!(%url, ?delay, "reconnecting after backoff");
			// Drain-aware: a GOAWAY off a healthy session continues straight to the
			// replacement dial, so a predecessor can still be draining when that dial
			// fails and lands here. A plain sleep would stop enforcing its handover
			// until some later dial succeeded, holding an upstream that asked to drain
			// open for the whole retry window, or forever with `--backoff-timeout=0`.
			sleep_draining(delay, &mut draining, shared).await;
			delay = pacing.next(delay);
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
	/// [`closed`](Self::closed); reach for [`session`](Self::session) afterwards if you
	/// need the transport itself.
	///
	/// It consumes and returns the connection so the reconnecting chain,
	/// `client.connect(url).established().await?`, keeps it: the loop lives in this
	/// handle, so a version that handed back only the session would leave the caller
	/// holding a live transport that silently never redials.
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

	/// The live [`moq_net::Session`], or `None` while between sessions.
	///
	/// An escape hatch for callers that need the raw session (during a GOAWAY handover
	/// this is the old session, which is still serving). Sessions are refcounted, so a
	/// clone held here keeps that transport open even after the loop moves on.
	pub fn session(&self) -> Option<moq_net::Session> {
		self.state.read().session.clone()
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

/// Sleep for `delay` while keeping a draining predecessor's deadline enforced.
///
/// The drain is otherwise only polled from [`run_session`], which is reached only
/// after a successful connect, so a replacement that takes several backoff rounds
/// to reach would leave the old session holding its connection well past the
/// handover window.
async fn sleep_draining(delay: Duration, draining: &mut Option<Draining>, shared: &Shared) {
	let mut sleep = std::pin::pin!(tokio::time::sleep(delay));
	kio::wait(|waiter| {
		if let Some(old) = draining.as_mut()
			&& old.poll(waiter)
		{
			*draining = None;
			// Nothing is serving now: the predecessor closed or overstayed its
			// handover, and the replacement is still a dial away. Leaving the state
			// on `Migrating` would keep handing callers a session that is already
			// closed, with its stats and version, until the next connect overwrote it.
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
async fn run_session(
	shared: &Shared,
	session: &moq_net::Session,
	draining: &mut Option<Draining>,
	migrate: bool,
) -> Ended {
	let mut send = session.send_bandwidth();
	let mut recv = session.recv_bandwidth();
	let goaway = session.draining();
	let closed = session.closed();
	tokio::pin!(closed);

	kio::wait(|waiter| {
		poll_forward(&mut send, &shared.send_bw, waiter);
		poll_forward(&mut recv, &shared.recv_bw, waiter);

		// Retire the predecessor once it closes or overstays its handover window.
		if let Some(old) = draining.as_mut()
			&& old.poll(waiter)
		{
			*draining = None;
		}

		// Checked before the close arm: a GOAWAY means the session is still up, and
		// migrating from it is not the same as reconnecting after it died.
		if migrate && let Poll::Ready(Ok(msg)) = goaway.poll(waiter) {
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

impl Drop for Connection {
	fn drop(&mut self) {
		self.abort.abort();
	}
}

/// The terminal error read from a closed channel's final state.
fn terminal(state: &State) -> Error {
	match &state.error {
		Some(err) => err.clone(),
		None => Error::Reconnect("connection stopped".to_string()),
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
		assert_eq!(backoff.max(), Duration::from_secs(30));
		assert_eq!(backoff.timeout(), Duration::from_secs(300));
	}

	/// The linger outlives the give-up timeout (so the reconnect error surfaces
	/// first), and an unlimited-retry timeout lingers forever.
	#[test]
	fn test_backoff_linger() {
		let backoff = Backoff::default();
		assert_eq!(backoff.linger(), backoff.timeout() + Duration::from_secs(1));

		let unlimited = Backoff {
			timeout: Some(Duration::ZERO),
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
}
