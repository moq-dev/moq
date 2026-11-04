//! Client connect policy: which of a peer's addresses to dial, how long each dial may
//! take, when to back off and when to give up, and how to follow a GOAWAY.
//!
//! [`Reconnect`] is a poll machine that does no I/O. The caller dials each [`Attempt`]
//! it asks for, reports the result with [`Reconnect::connected`] or
//! [`Reconnect::failed`], and otherwise polls it with the current time. The machine
//! watches each live session for its close and GOAWAY itself, keeps a drained
//! predecessor serving through its handover, and drops neither on the caller's behalf
//! until it reports [`Action::Done`].

mod redirect;
#[cfg(test)]
mod tests;

pub use redirect::Redirect;

use std::{collections::VecDeque, task::Poll, time::Duration};

use rand::RngExt;
use url::Url;

use crate::{Error, Session, SessionError, auth, goaway, time::Instant};
use redirect::{Redirects, Shown, endpoint};

/// How long one address gets before the walk moves to the peer's next one.
///
/// A black-holed address answers nothing rather than refusing, so without a bound the
/// transport's idle timeout would decide how long the remaining addresses wait. Only an
/// attempt with a later address to fall back on is worth bounding: cutting the last one
/// short just turns a slow connect into a failed one, on every reconnect cycle.
const ATTEMPT: Duration = Duration::from_secs(5);

/// When to redial after a failure, and when to give up.
///
/// `initial` and `multiplier` set the wait before each retry, `max` caps it, and
/// `healthy` decides which sessions count as failed attempts. The delays carry jitter,
/// so a fleet knocked offline together does not reconnect in lockstep. `timeout` bounds
/// every failure; a settled rejection, malformed protocol input, or invalid local
/// configuration stops retrying. A GOAWAY naming no new URI is no failure: a restarting
/// peer sends one to each session until it goes, and each of those serves, so it
/// restarts the timeout instead.
///
/// A GOAWAY off a healthy session migrates at once, unless it sends the connection back
/// to an endpoint that already redirected it elsewhere and the session lived less than
/// `max`. Two peers redirecting to each other thus wait out this backoff and count
/// against the timeout.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct Backoff {
	/// Delay before the first redial. Later delays grow from it, so zero turns retry
	/// waiting off entirely; an immediate close still counts as a failed attempt and
	/// spends the timeout. Defaults to 1 s.
	pub initial: Duration,
	/// Growth of the delay after each failure. 0 or 1 keeps it constant. Defaults to 2.
	pub multiplier: u32,
	/// Cap on the delay, which caps `initial` too. Zero leaves it uncapped. Defaults to 5 s.
	pub max: Duration,
	/// How long a session must stay up to count as healthy. Must be nonzero. Defaults to 1 s.
	///
	/// A healthy session's end resets the delay and the timeout. One that ends sooner is a
	/// failed attempt, so a peer that accepts and then drops every session still escalates
	/// the delay and exhausts the timeout.
	pub healthy: Duration,
	/// How long to keep retrying before giving up. Zero retries forever. Defaults to 10 s.
	///
	/// No new walk of the peer's addresses starts once it has passed, but one under way
	/// finishes: each address but the last gets up to 5 s, and the last runs to
	/// [`Config::timeout`], so a handshake slower than this still lands. Giving up can
	/// therefore take up to one whole walk longer. Resets after a healthy session outside a
	/// redirect loop.
	pub timeout: Duration,
}

impl Default for Backoff {
	fn default() -> Self {
		Self {
			initial: Duration::from_secs(1),
			multiplier: 2,
			max: Duration::from_secs(5),
			healthy: Duration::from_secs(1),
			timeout: Duration::from_secs(10),
		}
	}
}

/// How a connection reacts to a peer's GOAWAY.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct Goaway {
	/// What to do with the URI the GOAWAY names. Defaults to [`Redirect::SameHost`].
	pub redirect: Redirect,
	/// The longest the old session keeps serving after a GOAWAY. Defaults to 10 s.
	///
	/// It closes sooner, once its replacement is up and it carries no subscription or
	/// fetch. A GOAWAY naming a shorter deadline wins, since the peer force-closes at its
	/// own deadline regardless, but a longer one does not extend it.
	pub handover: Duration,
}

impl Default for Goaway {
	fn default() -> Self {
		Self {
			redirect: Redirect::default(),
			handover: Duration::from_secs(10),
		}
	}
}

impl Goaway {
	/// The handover window for a GOAWAY naming `deadline`: whichever is sooner.
	fn handover(&self, deadline: Option<Duration>) -> Duration {
		deadline.map_or(self.handover, |deadline| deadline.min(self.handover))
	}
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

/// The whole connect policy for one peer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct Config {
	/// When to redial, and when to give up.
	pub backoff: Backoff,
	/// How to follow a peer's GOAWAY.
	pub goaway: Goaway,
	/// What a GOAWAY naming no URI does.
	pub restart: Restart,
	/// The deadline for one dial and its handshake. Zero waits forever. Defaults to 30 s.
	pub timeout: Duration,
	/// End when the first session ends instead of redialing. Defaults to false.
	///
	/// A GOAWAY then ends it cleanly: with no replacement to dial, leaving is the answer.
	pub once: bool,
	/// Whether the dial pins a certificate fingerprint, which verifies only the host it
	/// was configured for, so a GOAWAY may not move an encrypted dial to another host.
	pub pinned: bool,
}

impl Default for Config {
	fn default() -> Self {
		Self {
			backoff: Backoff::default(),
			goaway: Goaway::default(),
			restart: Restart::default(),
			timeout: Duration::from_secs(30),
			once: false,
			pinned: false,
		}
	}
}

/// One of a peer's addresses, as the caller dials it.
pub trait Target: Clone + From<Url> {
	/// The URL a dial reaches, which a GOAWAY redirect is judged against.
	fn url(&self) -> &Url;

	/// Whether the dial skips resolving the URL for fixed socket addresses, which a
	/// GOAWAY redirect cannot carry over.
	fn fixed(&self) -> bool;
}

impl Target for Url {
	fn url(&self) -> &Url {
		self
	}

	fn fixed(&self) -> bool {
		false
	}
}

/// What a failed dial means for the rest of the policy, judged by the caller.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum Verdict {
	/// This address failed; try the next, then back off and retry.
	Retry,
	/// This address can never work, such as an unsupported scheme. Give up unless
	/// another address fails transiently.
	Unusable,
	/// The peer's answer for every address, such as an HTTP 404: stop the walk, then
	/// back off and retry.
	Settled,
	/// The peer refused the credential: give up unless the tokens changed since the dial.
	Refused,
	/// Nothing a retry can repair, such as malformed protocol input: give up.
	Fatal,
}

impl From<&Error> for Verdict {
	/// Judge an error from the MoQ handshake or a session's close.
	fn from(err: &Error) -> Self {
		match SessionError::from(err) {
			SessionError::Unauthorized | SessionError::Expired | SessionError::MalformedAuthToken => Self::Refused,
			SessionError::ProtocolViolation | SessionError::KeyValueFormatting | SessionError::InvalidRequestId => {
				Self::Fatal
			}
			_ => Self::Retry,
		}
	}
}

/// A failure the policy itself decides on.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum Failure {
	/// An address with another to fall back on did not connect in time.
	#[error("timed out connecting to {0}")]
	Attempt(String),
	/// A dial and its handshake outlasted [`Config::timeout`].
	#[error("connect timed out after {0:?}")]
	Timeout(Duration),
	/// The retry window ran out; carries the last failure's message.
	#[error("reconnect timed out after {after:?}{}", last.as_ref().map(|last| format!(": {last}")).unwrap_or_default())]
	Exhausted {
		/// The retry window.
		after: Duration,
		/// Why the last attempt failed, if one did.
		last: Option<String>,
	},
	/// Peers redirected the connection around in a loop.
	#[error("redirect loop")]
	Loop,
	/// The peer redirected a session elsewhere before it was healthy.
	#[error("peer redirected immediately")]
	Redirected,
	/// The redirect a GOAWAY named breaks the [`Redirect`] policy; carries why.
	#[error("refused GOAWAY redirect: {0}")]
	Refused(&'static str),
	/// A GOAWAY named a redirect for a target dialing fixed socket addresses.
	#[error("a GOAWAY redirected a target dialing fixed socket addresses")]
	Fixed,
}

/// Invalid input to [`Reconnect::new`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum ConfigError {
	/// There is no address to dial.
	#[error("no target to dial")]
	Empty,
	/// [`Backoff::healthy`] is zero, which would count every session as healthy and reset
	/// the timeout forever under a peer that drops each one on sight.
	#[error("the healthy threshold must be nonzero")]
	Healthy,
}

/// One dial the caller runs for [`Reconnect`].
#[derive(Clone, Debug)]
pub struct Attempt<A> {
	target: A,
	url: Url,
	token: Option<crate::setup::Token>,
}

impl<A> Attempt<A> {
	/// The address to dial.
	pub fn target(&self) -> &A {
		&self.target
	}

	/// The URL to dial, carrying the connection credential in its query when an offered
	/// version has no SETUP token.
	pub fn url(&self) -> &Url {
		&self.url
	}

	/// The connection credential for [`Client::with_token`](crate::Client::with_token), if any.
	pub fn token(&self) -> Option<&crate::setup::Token> {
		self.token.as_ref()
	}
}

/// What [`Reconnect::poll`] asks of the caller.
///
/// Exhaustive on purpose: a new action is one every driver must handle.
#[derive(Debug)]
pub enum Action<A, E> {
	/// Dial this attempt and report it with [`Reconnect::connected`] or
	/// [`Reconnect::failed`], polling meanwhile. Building each session on the tokens'
	/// handle, from [`Reconnect::with_tokens`], is the caller's.
	Dial(Attempt<A>),
	/// Drop the dial in flight: it ran out its deadline, which already counted as its failure.
	Abandon,
	/// The live session got a GOAWAY and now drains as the [`predecessor`](Reconnect::predecessor)
	/// while a replacement is dialed.
	Migrating,
	/// The predecessor closed or overstayed its handover.
	Retired,
	/// Nothing serves anymore: the live session ended with this error, or the predecessor
	/// retired between sessions. A redial follows.
	Offline(Option<E>),
	/// Why the policy is waiting to redial: a walk failed or a session ended too soon.
	Retrying(E),
	/// Poll again by this instant, or on a wake.
	Wait(Option<Instant>),
	/// The policy gave up with an error, or ended cleanly. Every later poll waits forever.
	Done(Result<(), E>),
}

/// Client connect policy as a poll machine; see the [module](self) docs.
///
/// `A` is the caller's address type and `E` its error type, which takes the
/// [`Error`] a session closes with and the [`Failure`]s the policy decides on.
pub struct Reconnect<A, E> {
	config: Config,
	targets: Vec<A>,
	tokens: Option<(auth::Tokens, crate::Versions)>,
	retry: Retry<E>,
	redirects: Redirects,
	phase: Phase,
	/// The failures of the walk under way.
	walk: Walk<E>,
	/// An old session kept serving after a GOAWAY so its in-flight groups finish.
	predecessor: Option<Predecessor>,
	queue: VecDeque<Action<A, E>>,
}

enum Phase {
	/// Check the retry window, then walk the targets.
	Start,
	/// Ask the caller to dial the target at `index`.
	Next {
		index: usize,
	},
	Dialing(Dialing),
	Live(Box<Live>),
	Backoff {
		until: Instant,
		/// After a restart's wait, the retry window counts from its end.
		restart: bool,
	},
	/// The policy ended; every later poll waits forever.
	Done,
}

struct Dialing {
	index: usize,
	deadline: Option<(Instant, Failure)>,
	presented: Option<auth::Dial>,
}

struct Live {
	session: Session,
	since: Instant,
	index: usize,
	/// Dropped once a restart is waited out: it is a latch, so it would keep firing.
	goaway: Option<goaway::Consumer>,
	presented: Option<auth::Dial>,
}

struct Walk<E> {
	/// The latest failure a retry may repair.
	transient: Option<E>,
	/// The latest failure of any kind.
	last: Option<E>,
}

impl<E> Default for Walk<E> {
	fn default() -> Self {
		Self {
			transient: None,
			last: None,
		}
	}
}

/// Why a live session stopped being the live one.
enum Ended {
	Closed(Error),
	/// The peer sent a GOAWAY; the session is still up and serving.
	Goaway(goaway::Goaway),
}

impl<A: Target, E: Clone + std::fmt::Display + From<Error> + From<Failure>> Reconnect<A, E> {
	/// Walk `targets` in order under `config`, starting at `now`.
	pub fn new(config: Config, targets: Vec<A>, now: Instant) -> Result<Self, ConfigError> {
		if targets.is_empty() {
			return Err(ConfigError::Empty);
		}
		if config.backoff.healthy.is_zero() {
			return Err(ConfigError::Healthy);
		}
		Ok(Self {
			retry: Retry::new(Pacing::new(&config.backoff).initial, now),
			config,
			targets,
			tokens: None,
			redirects: Redirects::default(),
			phase: Phase::Start,
			walk: Walk::default(),
			predecessor: None,
			queue: VecDeque::new(),
		})
	}

	/// Present `tokens` on every dial offering `versions`.
	///
	/// A refused credential then earns another dial once the tokens change, and an ended
	/// tokens handle ends the policy at its next walk.
	pub fn with_tokens(mut self, tokens: auth::Tokens, versions: crate::Versions) -> Self {
		self.tokens = Some((tokens, versions));
		self
	}

	/// The live session, if one is serving and has not received a GOAWAY.
	pub fn session(&self) -> Option<&Session> {
		match &self.phase {
			Phase::Live(live) => Some(&live.session),
			_ => None,
		}
	}

	/// The addresses the next walk dials, in order: the configured ones, or the one a
	/// GOAWAY redirected to.
	pub fn targets(&self) -> &[A] {
		&self.targets
	}

	/// The session draining after a GOAWAY, and the deadline its handover ends by.
	pub fn predecessor(&self) -> Option<(&Session, Instant)> {
		self.predecessor
			.as_ref()
			.map(|predecessor| (&predecessor.session, predecessor.deadline))
	}

	/// Advance to `now`, returning what the caller must do next.
	pub fn poll(&mut self, now: Instant, waiter: &kio::Waiter) -> Action<A, E> {
		loop {
			if let Some(action) = self.queue.pop_front() {
				return action;
			}

			let live = matches!(self.phase, Phase::Live(_));
			if let Some(predecessor) = self.predecessor.as_mut() {
				// The replacement serves, so a predecessor carrying nothing closes now.
				if live {
					predecessor.poll_idle(waiter);
				}
				if predecessor.poll(now, waiter) {
					self.predecessor = None;
					self.queue.push_back(Action::Retired);
					// Nothing is serving once it retires between sessions, so a caller must
					// stop handing out a session that is already closed.
					if !live {
						self.queue.push_back(Action::Offline(None));
					}
					continue;
				}
			}
			let handover = self.predecessor.as_ref().map(|predecessor| predecessor.deadline);

			match &mut self.phase {
				Phase::Start => self.start(now),
				Phase::Next { index } => {
					let index = *index;
					return self.dial(now, index);
				}
				Phase::Dialing(dialing) => match &dialing.deadline {
					Some((at, failure)) if now >= *at => {
						let failure = failure.clone();
						self.fail(now, failure.into(), Verdict::Retry);
						return Action::Abandon;
					}
					deadline => return Action::Wait(earliest(deadline.as_ref().map(|(at, _)| *at), handover)),
				},
				Phase::Live(live) => {
					let mut ended = None;
					// Checked before the close: a GOAWAY means the session is still up, and
					// migrating from it is not the same as reconnecting after it died.
					if let Some(Poll::Ready(Ok(msg))) = live.goaway.as_ref().map(|goaway| goaway.poll(waiter)) {
						if self.config.restart == Restart::Wait && msg.uri().is_empty() {
							tracing::info!("peer is restarting; serving until it closes the session");
							live.goaway = None;
						} else {
							ended = Some(Ended::Goaway(msg));
						}
					}
					if ended.is_none()
						&& let Poll::Ready(err) = live.session.poll_closed(waiter)
					{
						ended = Some(Ended::Closed(err));
					}
					match ended {
						Some(ended) => self.end(now, ended),
						None => return Action::Wait(handover),
					}
				}
				Phase::Backoff { until, restart } => {
					if now < *until {
						return Action::Wait(earliest(Some(*until), handover));
					}
					let restart = *restart;
					self.retry.delay = Pacing::new(&self.config.backoff).next(self.retry.delay);
					if restart {
						// After the wait, so the window counts only the time after the peer
						// stops answering.
						self.retry.start = now;
					}
					self.phase = Phase::Start;
				}
				Phase::Done => return Action::Wait(None),
			}
		}
	}

	/// Report that the dial asked for connected `session`, at `now`.
	///
	/// Ignored unless a dial is in flight, as after [`Action::Abandon`].
	pub fn connected(&mut self, now: Instant, session: Session) {
		if !matches!(self.phase, Phase::Dialing(_)) {
			return;
		}
		let Phase::Dialing(dialing) = std::mem::replace(&mut self.phase, Phase::Start) else {
			unreachable!("checked above");
		};
		if let Some((tokens, _)) = &self.tokens {
			tokens.connected(&session);
		}
		self.walk = Walk::default();
		self.phase = Phase::Live(Box::new(Live {
			goaway: Some(session.draining()),
			session,
			since: now,
			index: dialing.index,
			presented: dialing.presented,
		}));
	}

	/// Report that the dial asked for failed with `err`, judged as `verdict`.
	///
	/// Ignored unless a dial is in flight, as after [`Action::Abandon`].
	pub fn failed(&mut self, now: Instant, err: E, verdict: Verdict) {
		if matches!(self.phase, Phase::Dialing(_)) {
			self.fail(now, err, verdict);
		}
	}

	/// Start a walk, unless the retry window is spent or the tokens ended.
	fn start(&mut self, now: Instant) {
		let timeout = self.config.backoff.timeout;
		if !timeout.is_zero() && now.saturating_duration_since(self.retry.start) >= timeout {
			return self.done(Err(self.exhausted()));
		}
		// An ended auth handle refuses every later session, so no dial can recover. A fresh
		// watch only reports an error once its handle has ended.
		if let Some((tokens, _)) = &self.tokens
			&& let Poll::Ready(Err(err)) = tokens.handle().grant().poll_changed(&kio::Waiter::noop())
		{
			return self.done(Err(err.into()));
		}
		self.phase = Phase::Next { index: 0 };
	}

	fn dial(&mut self, now: Instant, index: usize) -> Action<A, E> {
		let target = self.targets[index].clone();
		tracing::info!(peer = %Shown(target.url()), "connecting");

		// Read before the dial starts its session's driver, so a request the session sends
		// early waits on the held tokens' answers rather than being refused outside the
		// connection credential's grant.
		let presented = self
			.tokens
			.as_ref()
			.map(|(tokens, versions)| tokens.dial(target.url(), versions));
		let (url, token) = match &presented {
			Some(presented) => (presented.url.clone(), presented.token.clone()),
			None => (target.url().clone(), None),
		};

		// The connect deadline covers the dial and the handshake, for every transport: it is
		// the only bound some of them have. The fixed bound applies only with a fallback.
		let connect = Some(self.config.timeout).filter(|timeout| !timeout.is_zero());
		let attempt = (index + 1 < self.targets.len()).then_some(ATTEMPT);
		let deadline = match (attempt, connect) {
			(Some(attempt), Some(connect)) if connect <= attempt => Some((now + connect, Failure::Timeout(connect))),
			(Some(attempt), _) => Some((now + attempt, Failure::Attempt(Shown(target.url()).to_string()))),
			(None, Some(connect)) => Some((now + connect, Failure::Timeout(connect))),
			(None, None) => None,
		};

		self.phase = Phase::Dialing(Dialing {
			index,
			deadline,
			presented,
		});
		Action::Dial(Attempt { target, url, token })
	}

	/// The dial in flight failed: move on to the next target, or end the walk.
	fn fail(&mut self, now: Instant, err: E, verdict: Verdict) {
		let Phase::Dialing(dialing) = std::mem::replace(&mut self.phase, Phase::Start) else {
			unreachable!("checked by the caller");
		};
		match verdict {
			// The peer's answer, not this address's, so unless it invites another attempt it
			// settles the whole walk. Carrying on would offer the same rejected credentials at
			// the peer's other addresses, and a later transport failure would overwrite the
			// real reason.
			Verdict::Settled | Verdict::Refused | Verdict::Fatal => {
				return self.walk_failed(now, err, verdict, dialing.presented.as_ref());
			}
			Verdict::Retry => self.walk.transient = Some(err.clone()),
			Verdict::Unusable => {}
		}
		tracing::debug!(peer = %Shown(self.targets[dialing.index].url()), %err, "address unreachable");
		self.walk.last = Some(err);

		if dialing.index + 1 < self.targets.len() {
			self.phase = Phase::Next {
				index: dialing.index + 1,
			};
			return;
		}

		// One unusable target must not turn another target's transient failure terminal.
		let walk = std::mem::take(&mut self.walk);
		match walk.transient {
			Some(err) => self.walk_failed(now, err, Verdict::Retry, None),
			None => self.walk_failed(now, walk.last.expect("a failed walk"), Verdict::Unusable, None),
		}
	}

	fn walk_failed(&mut self, now: Instant, err: E, verdict: Verdict, presented: Option<&auth::Dial>) {
		self.walk = Walk::default();
		let terminal = match verdict {
			Verdict::Fatal | Verdict::Unusable => true,
			Verdict::Refused => !self.changed(presented),
			Verdict::Retry | Verdict::Settled => false,
		};
		if terminal || self.config.once {
			return self.done(Err(err));
		}
		self.queue.push_back(Action::Retrying(err.clone()));
		self.retry.last = Some(err);
		// No URL here: with several targets there isn't one to name, and each attempt
		// already logged the address it tried.
		self.back_off(now, self.config.backoff.timeout, false, "reconnecting after backoff");
	}

	/// Score how a live session ended and decide what follows.
	fn end(&mut self, now: Instant, ended: Ended) {
		let Phase::Live(live) = std::mem::replace(&mut self.phase, Phase::Start) else {
			unreachable!("only a live session ends");
		};
		let tenure = now.saturating_duration_since(live.since);
		let url = self.targets[live.index].url().clone();
		let pacing = Pacing::new(&self.config.backoff);

		// The connected target owns the policy, including when connecting once. A refused
		// redirect is terminal: the peer is leaving and named somewhere we won't go, so
		// redialing the old address or a fallback would ignore it.
		let assigned = match &ended {
			Ended::Goaway(msg) => match self.config.goaway.redirect.target(msg.uri(), &url, self.config.pinned) {
				Ok(assigned) => assigned,
				Err(failure) => return self.done(Err(failure.into())),
			},
			Ended::Closed(_) => None,
		};
		if assigned.is_some() && self.targets[live.index].fixed() {
			return self.done(Err(Failure::Fixed.into()));
		}

		// Connecting once leaves rather than migrating: there is no replacement to dial, and
		// a GOAWAY naming no deadline never force-closes, so the peer stops accepting
		// requests and waits for us. Ignoring it is how both sides end up waiting on each
		// other forever.
		if self.config.once && matches!(ended, Ended::Goaway(_)) {
			return self.done(Ok(()));
		}

		// Two peers that each hold a session past the healthy bar can still bounce us between
		// them forever. Returning to an endpoint that already sent us elsewhere in this run of
		// migrations skips the healthy-session reset, so the backoff escalates and the retry
		// window still applies. A one-way rolling restart never revisits one, and a GOAWAY
		// back to the same place is a restart, not a move, so it never counts. A session that
		// outlived the capped delay already redials no faster than the backoff would allow, so
		// it starts a new run: rolling restarts days apart never add up to a loop.
		if matches!(ended, Ended::Closed(_)) || tenure >= pacing.settled() {
			self.redirects.reset();
		}
		// Only an accepted redirect is a move: an empty or ignored one reconnects in place.
		let looped = assigned.as_ref().is_some_and(|target| {
			let from = endpoint(&url);
			let to = endpoint(target);
			to != from && self.redirects.depart(from, &to)
		});

		// A session that stayed up past the healthy bar is healthy; one that ended sooner, or
		// carried on a redirect loop, counts as a failed attempt.
		let healthy = !looped && self.retry.settle(now, tenure, &pacing);

		let msg = match ended {
			Ended::Goaway(msg) => msg,
			Ended::Closed(err) => return self.closed(now, err, healthy, live.presented.as_ref(), &url),
		};

		// An accepted redirect is an assignment: keep dialing it from here on, and only it.
		// The peer named exactly one place to go, which retires whatever other addresses got
		// us to this session.
		let url = match assigned {
			Some(target) => {
				self.targets = vec![A::from(target.clone())];
				target
			}
			None => url,
		};

		// Hand over gracefully however the backoff scores this session. The old one keeps
		// serving until it closes or overstays, so its routes stay attached and live tracks
		// splice onto the replacement at a group boundary.
		tracing::info!(peer = %Shown(&url), "upstream GOAWAY; migrating");
		if let Some(old) = self.predecessor.take() {
			old.retire();
		}
		self.predecessor = Some(Predecessor {
			deadline: now + self.config.goaway.handover(msg.timeout()),
			session: live.session,
			closing: false,
		});
		self.queue.push_back(Action::Migrating);

		// No backoff: a handover off a healthy session is not a failure.
		if healthy {
			return;
		}

		// Drained almost immediately, or looped back. The backoff still escalates, so the
		// redials cannot hammer the peer, and the old session serves across the wait.
		//
		// An empty URI is "reconnect to me": the peer is restarting and drains each new
		// session until it goes, which is no failure, since every one serves. A named URI is
		// a redirect, scored as a failed attempt so two peers bouncing us between them
		// eventually give up.
		let restarting = msg.uri().is_empty();
		if !restarting {
			let err: E = if looped { Failure::Loop } else { Failure::Redirected }.into();
			self.queue.push_back(Action::Retrying(err.clone()));
			self.retry.last = Some(err);
		}
		// A restart's pacing is not capped by the window: a delay longer than the window
		// would otherwise spend it all and end the loop without a redial.
		let window = if restarting {
			Duration::ZERO
		} else {
			self.config.backoff.timeout
		};
		let reason = if looped {
			"peer redirected back in a loop; retrying after backoff"
		} else {
			"peer drained the session immediately; retrying after backoff"
		};
		self.back_off(now, window, restarting, reason);
	}

	/// The live session closed with `err`.
	fn closed(&mut self, now: Instant, err: Error, healthy: bool, presented: Option<&auth::Dial>, url: &Url) {
		// Refusals and malformed protocol input are settled failures, however long the
		// session lived, unless the tokens changed since this dial read them.
		let terminal = match Verdict::from(&err) {
			Verdict::Fatal => true,
			Verdict::Refused => !self.changed(presented),
			_ => false,
		};
		let err = E::from(err);
		// Connecting once, the session ending ends the policy with its close reason.
		if terminal || self.config.once {
			return self.done(Err(err));
		}

		self.queue.push_back(Action::Offline(Some(err.clone())));
		if healthy {
			// The window was reset, so a one-off drop reconnects promptly.
			tracing::warn!(peer = %Shown(url), "session closed, reconnecting");
		} else {
			// Connected then dropped almost immediately, such as a server that accepts then
			// resets. Keep the reason so the give-up reports a real cause, and back off so
			// repeated flaps escalate instead of spinning.
			tracing::warn!(peer = %Shown(url), %err, "session severed immediately, retrying");
			self.retry.last = Some(err);
		}
		self.back_off(now, self.config.backoff.timeout, false, "reconnecting after backoff");
	}

	/// Wait out the next jittered delay, capped by what remains of `window`.
	fn back_off(&mut self, now: Instant, window: Duration, restart: bool, reason: &'static str) {
		let Some(wait) = self.retry.wait(now, window) else {
			return self.done(Err(self.exhausted()));
		};
		tracing::warn!(?wait, "{reason}");
		self.phase = Phase::Backoff {
			until: now + wait,
			restart,
		};
	}

	/// Whether the tokens changed since `presented` read them, so a refusal earns another dial.
	fn changed(&self, presented: Option<&auth::Dial>) -> bool {
		match (&self.tokens, presented) {
			(Some((tokens, _)), Some(presented)) => tokens.changed(presented),
			_ => false,
		}
	}

	fn exhausted(&self) -> E {
		Failure::Exhausted {
			after: self.config.backoff.timeout,
			last: self.retry.last.as_ref().map(ToString::to_string),
		}
		.into()
	}

	fn done(&mut self, result: Result<(), E>) {
		self.predecessor = None;
		self.phase = Phase::Done;
		self.queue.push_back(Action::Done(result));
	}
}

fn earliest(a: Option<Instant>, b: Option<Instant>) -> Option<Instant> {
	match (a, b) {
		(Some(a), Some(b)) => Some(a.min(b)),
		(a, b) => a.or(b),
	}
}

/// The retry pacing resolved from [`Backoff`].
struct Pacing {
	/// The first delay, already under the cap.
	initial: Duration,
	/// The delay cap, or `None` for uncapped.
	max: Option<Duration>,
	/// At least 1: a multiplier of zero means no growth, not a delay that shrinks to nothing.
	multiplier: u32,
	/// The bar a session must clear to count as healthy.
	healthy: Duration,
}

impl Pacing {
	fn new(backoff: &Backoff) -> Self {
		let max = Some(backoff.max).filter(|max| !max.is_zero());
		Self {
			initial: max.map_or(backoff.initial, |max| backoff.initial.min(max)),
			max,
			multiplier: backoff.multiplier.max(1),
			healthy: backoff.healthy,
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

/// The backoff window across a run of failed attempts.
struct Retry<E> {
	/// The wait before the next attempt, before jitter.
	delay: Duration,
	/// When this run of failures began; the give-up timeout counts from here.
	start: Instant,
	/// The latest failure, so giving up reports a real cause.
	last: Option<E>,
}

impl<E> Retry<E> {
	fn new(initial: Duration, now: Instant) -> Self {
		Self {
			delay: initial,
			start: now,
			last: None,
		}
	}

	/// Score how long a session served: past the healthy bar starts a fresh retry window.
	fn settle(&mut self, now: Instant, tenure: Duration, pacing: &Pacing) -> bool {
		let healthy = tenure >= pacing.healthy;
		if healthy {
			*self = Self::new(pacing.initial, now);
		}
		healthy
	}

	/// Jitter the next delay and cap it at what remains of `window`. `None` means the
	/// window is spent; a zero window never is.
	fn wait(&self, now: Instant, window: Duration) -> Option<Duration> {
		let wait = self.delay.mul_f64(0.5 + kio::clock::rng().random::<f64>() / 2.0);
		if window.is_zero() {
			return Some(wait);
		}
		Some(wait.min(window.checked_sub(now.saturating_duration_since(self.start))?))
	}
}

/// An old session kept serving after a GOAWAY so its in-flight groups finish.
struct Predecessor {
	session: Session,
	/// When its handover ends.
	deadline: Instant,
	/// Whether [`Self::poll_idle`] started the graceful close.
	closing: bool,
}

impl Predecessor {
	/// Close the old session now, whatever remains of its window.
	fn retire(self) {
		self.session.abort(Error::GoawayTimeout);
	}

	/// Close the old session gracefully once it carries no subscription or fetch.
	///
	/// Only while a replacement serves: until then everything new still lands here. The
	/// handover window still bounds the close.
	fn poll_idle(&mut self, waiter: &kio::Waiter) {
		if self.closing || self.session.poll_idle(waiter).is_pending() {
			return;
		}
		tracing::debug!("old session carries nothing after GOAWAY; closing");
		self.closing = true;
		self.session.start_close();
	}

	/// Poll the drain; `true` once it is over.
	fn poll(&mut self, now: Instant, waiter: &kio::Waiter) -> bool {
		if self.session.poll_closed(waiter).is_ready() {
			tracing::debug!("old session drained cleanly after GOAWAY");
			return true;
		}
		if now >= self.deadline {
			tracing::warn!("old session did not drain in time; closing");
			self.session.abort(Error::GoawayTimeout);
			return true;
		}
		false
	}
}
