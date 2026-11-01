//! In-band authorization: present tokens to the peer and learn what they grant.
//!
//! Each side of a session presents the credential its connection
//! already carried (the URL, a client certificate, or nothing) right after
//! setup, and learns the [`Grant`] it earned. `Session::auth`
//! returns the [`Handle`]: [`grant`](Handle::grant) is the union of every token
//! this side presented, and `add` presents another without
//! reconnecting. The returned [`Credential`] is cloneable; dropping its last clone withdraws it.
//! A [`Route`](crate::origin::Route) can name it, so the request advertising the route
//! carries it too, and swapping it renews that request in place.
//!
//! A handle passed to `Client::with_auth` outlives its sessions: each session built on it
//! presents every token it holds, so a route naming one of its [`Credential`]s keeps it across a
//! reconnect. Only the peer's verdict ends such a token; a session ending leaves it for the
//! next. It backs one active session at a time, plus the ones draining after a GOAWAY.
//!
//! The other direction, answering the peer's tokens, is automatic: the session
//! grants what its own origin handles allow. A server that verifies tokens itself
//! takes the client's from `server::Handshake::requests` before accepting, and then
//! answers every token the client presents. Either way,
//! [`authorize`](Handle::authorize) re-authorizes the peer on a live session, narrower
//! or wider, on every version, whether or not it speaks AUTH.
//!
//! Every token the peer presents is one kind of [`Request`], answered on the same queue,
//! wherever it rode: its own AUTH stream, or a moq-transport `AUTHORIZATION TOKEN` on a
//! SUBSCRIBE or PUBLISH_NAMESPACE. The session enforces each grant it issues. A request
//! that carried a token is held to that token's grant alone and ends with it; any other
//! request is held to the session's scope, which a token carried by a request never joins.
//!
//! moq-transport carries the same exchange when both sides negotiate the MoQ Auth
//! extension. Older moq-lite versions, and peers that do not negotiate it, carry no
//! AUTH exchange: there the grant stays `None`. On moq-transport `add` still succeeds,
//! with a grant that stays unknown, since a request that names the auth carries its
//! token; on moq-lite without AUTH it fails with [`Error::Unsupported`]. Both wires carry
//! a grant's patterns as they are, and refuse one too large for a single AUTH_OK rather
//! than trim it.

use std::{
	collections::{BTreeMap, VecDeque},
	task::Poll,
	time::Duration,
};

use bytes::Bytes;

use crate::{Error, Patterns, Result, SessionError, time::Instant};

mod tokens;

pub use tokens::{Dial, EmptyToken, Token, Tokens};

/// Classify malformed AUTH messages separately from transport and local failures.
pub(crate) fn protocol_error(err: Error) -> Error {
	match err {
		Error::Decode(_)
		| Error::BoundsExceeded(_)
		| Error::WrongSize
		| Error::UnexpectedMessage
		| Error::UnexpectedStream => Error::ProtocolViolation,
		err => err,
	}
}

/// `ready!` for a `kio::Shared` poll, which yields a guard rather than a result.
macro_rules! ready_or {
	($e:expr) => {
		match $e {
			Poll::Ready(guard) => guard,
			Poll::Pending => return Poll::Pending,
		}
	};
}

/// How a peer accepted a token [`Handle::add`] presented.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Answer {
	/// Granted on its AUTH stream: its grant joins the union.
	Granted,
	/// Taken only for the requests that name it, on moq-transport without AUTH.
	Requests,
}

/// What a peer lets this side do, in this side's own paths.
///
/// The paths are relative to the session's root, the same paths the session
/// announces and subscribes with. `Grant::default()` grants nothing.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Grant {
	/// The broadcasts this side may publish to the peer.
	pub publish: Patterns,
	/// The broadcasts this side may subscribe to from the peer.
	pub subscribe: Patterns,
	/// When the grant lapses, or `None` for never.
	pub expires: Option<Instant>,
}

impl Grant {
	/// A grant of everything that never expires.
	pub fn all() -> Self {
		Self {
			publish: crate::Pattern::all().into(),
			subscribe: crate::Pattern::all().into(),
			expires: None,
		}
	}

	/// The paths any of `grants` allows, lapsing at the earliest expiry, which is when
	/// the union next shrinks. Reduced once, so it costs about the grants' total size.
	fn union<'a>(grants: impl IntoIterator<Item = &'a Self>) -> Self {
		let mut publish = Vec::new();
		let mut subscribe = Vec::new();
		let mut expires = None;
		for grant in grants {
			publish.extend(grant.publish.iter().cloned());
			subscribe.extend(grant.subscribe.iter().cloned());
			expires = earliest(expires, grant.expires);
		}
		Self {
			publish: publish.into_iter().collect(),
			subscribe: subscribe.into_iter().collect(),
			expires,
		}
	}

	/// The paths both grants allow, lapsing at the earlier expiry.
	///
	/// Fails closed: an intersection too large to hold grants nothing, never more.
	fn intersect(&self, other: &Self) -> Self {
		let both = |a: &Patterns, b: &Patterns| {
			a.intersect(b).unwrap_or_else(|err| {
				tracing::warn!(%err, "grant intersection too large; granting nothing");
				Patterns::new()
			})
		};
		Self {
			publish: both(&self.publish, &other.publish),
			subscribe: both(&self.subscribe, &other.subscribe),
			expires: earliest(self.expires, other.expires),
		}
	}
}

/// The earlier of two optional deadlines, where `None` is never.
fn earliest(a: Option<Instant>, b: Option<Instant>) -> Option<Instant> {
	match (a, b) {
		(Some(a), Some(b)) => Some(a.min(b)),
		(a, b) => a.or(b),
	}
}

/// How long a token refused for a reason that may pass waits before its session presents it
/// again, doubling on each refusal up to [`RETRY_MAX`].
const RETRY_INITIAL: Duration = Duration::from_secs(1);
const RETRY_MAX: Duration = Duration::from_secs(30);

/// Whether a refusal is the acceptor failing to decide rather than its verdict on the
/// token, such as an unreachable auth server or a full verification queue.
fn transient(err: &Error) -> bool {
	matches!(
		err,
		Error::Session(SessionError::Internal | SessionError::TooManyRequests | SessionError::Timeout)
	)
}

/// Whether a token's stream ending with `err` is the peer's verdict on it: a refusal that
/// will not pass, [`Error::Unsupported`], or a clean close after a grant. Anything else
/// left the token undecided.
fn verdict(slot: &Slot, err: &Error) -> bool {
	(slot.refused && !transient(err))
		|| matches!(err, Error::Unsupported)
		// The peer ended the grant without revoking it.
		|| (matches!(err, Error::Cancel) && slot.grant.is_some())
}

/// The shared state behind one [`Handle`]: the tokens it presents, and every session
/// it backs.
#[derive(Default)]
pub(crate) struct State {
	/// Created by a session for itself, so it ends with that session.
	bound: bool,
	/// Set once the handle ends; every waiter resolves with it.
	closed: Option<Error>,
	next_id: u64,
	/// Every token the application added that it has not withdrawn.
	tokens: BTreeMap<u64, Entry>,
	next_link: u64,
	/// Every session built on this handle that has not dropped, oldest first.
	links: BTreeMap<u64, Attached>,
	/// What the peer's connection credential lets it do, in the peer's terms: `None` (only
	/// the origin handles bound it) until [`Handle::authorize`] sets it; the grant it was
	/// last called with.
	limit: Option<Grant>,
	next_issue: u64,
	acceptor: Acceptor,
	/// The tokens a [`Tokens`] presents on this handle's sessions, when it was built by one.
	configured: Option<tokens::Configured>,
}

/// A token the application added, presented on every session of its handle.
struct Entry {
	value: Bytes,
	/// The first verdict any session's peer gave: how it accepted the token, or why it ended.
	answered: Option<Result<Answer>>,
	/// Why the token ended for good: a peer's refusal or revocation, or the handle's end.
	ended: Option<Error>,
}

/// One session's side of a handle.
struct Attached {
	/// Whether the negotiated version carries AUTH at all.
	supported: bool,
	/// Whether a request can carry a token itself (moq-transport), so an auth needs no
	/// AUTH stream to be named.
	requests: bool,
	/// Set once the session ends.
	closed: Option<Error>,
	/// The session's GOAWAY: once either side sends one, the session is draining.
	goaway: Option<std::sync::Arc<crate::goaway::Handle>>,
	/// Every token this session presented that has not been withdrawn.
	slots: BTreeMap<u64, Slot>,
	/// Tokens waiting for the driver to open their stream.
	opening: VecDeque<u64>,
	/// The union of every open token's grant, `None` until the peer first replies.
	union: Option<Grant>,
	/// The peer replied to some token, so the union is known even when empty.
	replied: bool,
	/// The grants the application issued to the peer's AUTH tokens, by stream, in the
	/// peer's terms. A token on a single request never joins them.
	issued: BTreeMap<u64, Grant>,
	/// The session's scope for the peer: the limit and every issued grant, `None` while the
	/// limit is.
	scope: Option<Grant>,
	/// Bumped whenever the union or the scope changes, so the session's per-stream
	/// gates can skip re-matching paths on every wakeup.
	epoch: u64,
	/// Whether the peer has accepted the connection, credential included: at once, unless
	/// the session [awaits the peer's SETUP](Link::await_setup).
	accepted: bool,
}

/// One token as one session presents it.
struct Slot {
	token: Bytes,
	/// Presented by the session itself at setup, so enforcement waits on it.
	setup: bool,
	/// The latest AUTH_OK, `None` before the first and after the token ends.
	grant: Option<Grant>,
	/// Handed to the driver to open its AUTH stream, which then owns ending it.
	streamed: bool,
	/// The latest reply: `Ok` for an AUTH_OK, the refusal otherwise.
	answered: Option<Result<()>>,
	/// The peer refused the current attempt with an AUTH_ERROR.
	refused: bool,
	/// The delay before the last retry, which the next one doubles.
	retry: Option<Duration>,
	/// Why the token ended on this session, once it has.
	ended: Option<Error>,
	/// The last [`Credential`] clone dropped: the driver closes the stream.
	withdrawn: bool,
}

impl Slot {
	fn new(token: Bytes, setup: bool, answered: Option<Result<()>>) -> Self {
		let ended = match &answered {
			Some(Err(err)) => Some(err.clone()),
			_ => None,
		};
		Self {
			token,
			setup,
			grant: None,
			streamed: false,
			answered,
			refused: false,
			retry: None,
			ended,
			withdrawn: false,
		}
	}
}

/// Who answers the peer's tokens. Decided once, when a driver first runs.
#[derive(Default)]
enum Acceptor {
	/// Nobody asked yet; the app may still take [`Handle::requests`].
	#[default]
	Undecided,
	/// The app answers every token.
	App(Pending),
	/// The session answers from its own origin handles.
	Default,
}

impl Attached {
	fn new(supported: bool, requests: bool, limit: Option<&Grant>) -> Self {
		let mut attached = Self {
			supported,
			requests,
			closed: None,
			goaway: None,
			slots: BTreeMap::new(),
			opening: VecDeque::new(),
			union: None,
			replied: false,
			issued: BTreeMap::new(),
			scope: None,
			epoch: 0,
			accepted: true,
		};
		attached.rescope(limit);
		attached
	}

	/// Whether the session still takes new work: open, with no GOAWAY sent or received.
	fn active(&self) -> bool {
		self.closed.is_none() && !self.goaway.as_ref().is_some_and(|goaway| goaway.draining())
	}

	/// Present the handle's token `id` on this session, returning the answer when the
	/// session settles it at once: `Ok` when only a request can carry it, so nothing will
	/// answer, and [`Error::Unsupported`] when nothing can.
	///
	/// A draining session takes no new tokens, so its verdict never ends one its successor
	/// carries.
	fn offer(&mut self, id: u64, token: &Bytes) -> Option<Result<()>> {
		if !self.active() {
			return None;
		}
		let answered = match (self.supported, self.requests) {
			(true, _) => None,
			(false, true) => Some(Ok(())),
			(false, false) => Some(Err(Error::Unsupported)),
		};
		self.slots.insert(id, Slot::new(token.clone(), false, answered.clone()));
		if self.supported {
			self.opening.push_back(id);
		}
		answered
	}

	/// End token `id` on this session, if it is still live here.
	fn end(&mut self, id: u64, err: &Error) {
		let Some(slot) = self.slots.get_mut(&id) else {
			return;
		};
		if slot.ended.is_some() {
			return;
		}
		slot.grant = None;
		slot.answered.get_or_insert(Err(err.clone()));
		slot.ended = Some(err.clone());
		self.epoch += 1;
		self.recompute();
	}

	/// The last [`Credential`] clone for token `id` dropped.
	fn withdraw(&mut self, id: u64) {
		let Some(slot) = self.slots.get_mut(&id) else {
			return;
		};
		slot.withdrawn = true;
		// Already over, or no stream whose close would end it: nothing left to close.
		if slot.ended.is_some() || !slot.streamed {
			self.opening.retain(|opening| *opening != id);
			self.slots.remove(&id);
			self.recompute();
		}
	}

	fn recompute(&mut self) {
		let mut granted = self.slots.values().filter_map(|slot| slot.grant.as_ref()).peekable();
		if granted.peek().is_none() && !self.replied {
			return;
		}
		let union = Grant::union(granted);
		if self.union.as_ref() != Some(&union) {
			self.union = Some(union);
			self.epoch += 1;
		}
	}

	/// Re-derive the scope from the handle's limit and the issued grants.
	fn rescope(&mut self, limit: Option<&Grant>) {
		let scope = limit.map(|limit| Grant::union(std::iter::once(limit).chain(self.issued.values())));
		if self.scope != scope {
			self.scope = scope;
			self.epoch += 1;
		}
	}

	/// The union's and the scope's patterns for `direction`, each `None` while
	/// unrestricted.
	fn parts(&self, direction: Direction) -> (Option<&Patterns>, Option<&Patterns>) {
		// The peer's grant names what we may do; the scope names what the peer may,
		// so what we send is what the peer may subscribe to, and the other way around.
		match direction {
			Direction::Publish => (
				self.union.as_ref().map(|union| &union.publish),
				self.scope.as_ref().map(|scope| &scope.subscribe),
			),
			Direction::Subscribe => (
				self.union.as_ref().map(|union| &union.subscribe),
				self.scope.as_ref().map(|scope| &scope.publish),
			),
		}
	}

	/// What `direction` allows right now.
	fn permit(&self, direction: Direction) -> Permit {
		let (granted, limit) = self.parts(direction);
		Permit {
			granted: granted.cloned(),
			limit: limit.cloned(),
		}
	}

	/// Whether `direction` allows `path` right now, without copying the patterns.
	fn allows(&self, direction: Direction, path: &str) -> bool {
		let (granted, limit) = self.parts(direction);
		granted.is_none_or(|granted| granted.matches(path)) && limit.is_none_or(|limit| limit.matches(path))
	}
}

impl State {
	/// The session the handle backs now: the newest one still open.
	fn live(&self) -> Option<&Attached> {
		self.links.values().rev().find(|link| link.closed.is_none())
	}

	/// Record a session's answer to token `id`: an acceptance settles the application's `add`,
	/// and a refusal is the verdict that ends the token on every session.
	fn answer(&mut self, id: u64, answer: Result<Answer>) {
		match answer {
			Ok(answer) => {
				if let Some(token) = self.tokens.get_mut(&id) {
					token.answered.get_or_insert(Ok(answer));
				}
				if answer == Answer::Requests {
					self.unsupported(id);
				}
				self.settle();
			}
			Err(err) => self.end_token(id, err),
		}
	}

	/// End token `id` for good, closing its stream on every session.
	fn end_token(&mut self, id: u64, err: Error) {
		let Some(token) = self.tokens.get_mut(&id) else {
			return;
		};
		if token.ended.is_some() {
			return;
		}
		token.answered.get_or_insert(Err(err.clone()));
		token.ended = Some(err.clone());
		for link in self.links.values_mut() {
			link.end(id, &err);
		}
		if self.closed.is_none() && self.configured(id) {
			match err {
				Error::Unsupported => self.unsupported(id),
				err => tracing::warn!(%err, "a configured token was refused"),
			}
		}
		self.settle();
	}

	/// Register a token and offer it to every open session.
	fn present(&mut self, token: Bytes) -> u64 {
		let id = self.next_id;
		self.next_id += 1;
		self.tokens.insert(
			id,
			Entry {
				value: token.clone(),
				answered: None,
				ended: None,
			},
		);
		let answers: Vec<_> = self
			.links
			.values_mut()
			.filter_map(|link| link.offer(id, &token))
			.collect();
		for answer in answers {
			self.answer(id, answer.map(|()| Answer::Requests));
		}
		id
	}

	/// Stop presenting token `id` on every session.
	fn withdraw(&mut self, id: u64) {
		self.tokens.remove(&id);
		for link in self.links.values_mut() {
			link.withdraw(id);
		}
	}

	/// End the handle, returning the application's queue for the caller to close once the
	/// lock is released, since refusing its tokens takes the lock again.
	fn abort(&mut self, err: Error) -> Option<Pending> {
		if self.closed.is_some() {
			return None;
		}
		self.record();
		self.closed = Some(err.clone());
		let ids: Vec<u64> = self.tokens.keys().copied().collect();
		for id in ids {
			self.end_token(id, err.clone());
		}
		match &self.acceptor {
			Acceptor::App(queue) => Some(queue.clone()),
			_ => None,
		}
	}
}

/// What one direction of a session allows: the grant the peer gave this side (the
/// union of its tokens) and the limit this side set on the peer, each `None`
/// while unrestricted.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Permit {
	granted: Option<Patterns>,
	limit: Option<Patterns>,
}

impl Permit {
	/// Whether both allow `path`.
	pub(crate) fn matches(&self, path: &str) -> bool {
		self.granted(path) && self.within_limit(path)
	}

	/// Whether the peer's grant alone allows `path`.
	pub(crate) fn granted(&self, path: &str) -> bool {
		self.granted.as_ref().is_none_or(|granted| granted.matches(path))
	}

	/// Whether the limit alone allows `path`: for what the peer offers us, which our
	/// own grant does not decide.
	pub(crate) fn within_limit(&self, path: &str) -> bool {
		self.limit.as_ref().is_none_or(|limit| limit.matches(path))
	}

	/// Whether this allows everything `other` does, judged part by part, so a `false`
	/// may be spurious but a `true` never is.
	pub(crate) fn covers(&self, other: &Self) -> bool {
		let covers = |this: &Option<Patterns>, other: &Option<Patterns>| match (this, other) {
			(None, _) => true,
			(Some(_), None) => false,
			(Some(this), Some(other)) => this.covers(other),
		};
		covers(&self.granted, &other.granted) && covers(&self.limit, &other.limit)
	}
}

/// The tokens this side presents and the grant they earn, across every session built on it.
///
/// A session made without one creates its own, returned by `Session::auth`, which ends
/// with that session. Pass one to `Client::with_auth` instead and it outlives its
/// sessions: each presents every token the handle holds, so a [`Credential`] it issued keeps
/// riding a route's announcements across a reconnect. It backs one active session at a
/// time, plus the ones draining after a GOAWAY.
///
/// Cheap to clone; every clone shares the same tokens.
#[derive(Clone, Default)]
pub struct Handle {
	state: kio::Shared<State>,
}

impl Handle {
	/// A handle backing no session yet, for `Client::with_auth`.
	pub fn new() -> Self {
		Self::default()
	}

	/// The union of every grant this side holds on the live session: `None` until its peer
	/// first answers a token (with a grant or a refusal), while no session is live, and
	/// forever on a version without AUTH.
	pub fn grant(&self) -> Watch {
		Watch::new(self.state.clone(), Selector::Union)
	}

	/// Present another token on every session of this handle, resolving once a peer accepts it
	/// with how it did.
	///
	/// The grant joins the union while any clone of the returned [`Credential`] lives. With no
	/// session live it waits for the next one. A refusal that may pass, such as an
	/// unreachable auth server, is presented again after a backoff, and so is a token whose
	/// stream the peer drops without a verdict. On moq-transport
	/// without AUTH there is no answer to wait for: it resolves at once with
	/// [`Answer::Requests`] and a grant that stays unknown, and only a request that names it
	/// carries the token.
	///
	/// # Errors
	///
	/// [`Error::Unsupported`] when neither the session nor its requests can carry a token
	/// (moq-lite without AUTH, or a peer that takes no tokens in band), the peer's refusal
	/// as [`Error::Session`], or the handle's end.
	#[cfg(feature = "async")]
	pub async fn add(&self, token: impl Into<Bytes>) -> Result<(Credential, Answer)> {
		let auth = self.present(token.into())?;
		let answer = kio::wait(|waiter| auth.poll_answered(waiter)).await?;
		Ok((auth, answer))
	}

	/// Register a token and queue it on every open session.
	pub(crate) fn present(&self, token: Bytes) -> Result<Credential> {
		let mut guard = self.state.lock();
		let state = &mut *guard;
		if let Some(err) = &state.closed {
			return Err(err.clone());
		}
		// A draining session takes no new tokens, and a handle of its own has no successor.
		if state.bound && !state.links.values().any(Attached::active) {
			return Err(Error::GoingAway);
		}
		let id = state.present(token);
		Ok(Credential {
			presented: std::sync::Arc::new(Presented {
				state: self.state.clone(),
				id,
				link: None,
			}),
		})
	}

	/// Answer every token the peer presents, instead of the default acceptor; see
	/// [`Handshake::requests`](crate::server::Handshake::requests).
	///
	/// Called before the session's driver first polls, since that decides who answers.
	/// A later call hands the queue to the new [`Requests`], ending the earlier one.
	pub(crate) fn requests(&self) -> Requests {
		let mut state = self.state.lock();
		debug_assert!(
			!matches!(state.acceptor, Acceptor::Default),
			"the driver already answers the peer's tokens"
		);
		if let Acceptor::App(earlier) = &state.acceptor {
			earlier.close();
		}
		let queue = Pending::default();
		if state.closed.is_some() {
			queue.close();
		}
		state.acceptor = Acceptor::App(queue.clone());
		Requests { queue }
	}

	/// Authorize the peer's connection credential for `grant` on every session of this
	/// handle, in the session's own paths. Calling it again re-authorizes, narrower or
	/// wider, replacing the last grant. The session's origin handles still bound it, so it
	/// never grants more than they allow.
	///
	/// The session's scope for the peer is this grant and every grant issued to one of its
	/// AUTH tokens through `server::Handshake::requests`; a session never authorized is bound
	/// only by its origin handles. A request that carries its own token is decided by that
	/// token instead, inside the scope or not.
	///
	/// A narrower scope ends what falls outside at once, with [`Error::Unauthorized`]
	/// where it has a reader: announcements to the peer retract, its new requests are
	/// refused, its subscriptions reset, and the broadcasts it published abort. A wider
	/// one brings back what the old scope held back: announcements to the peer are made
	/// again, and so are the peer's announcements it withheld from the origin, and new
	/// requests are accepted. The rest of the session carries on. It works on every
	/// version, since the session enforces it without the peer's help.
	///
	/// When the session answers the peer's connection credential itself, the peer is
	/// told the grant, within what the origin handles allow, `expires` included. An
	/// application answering tokens through `server::Handshake::requests` updates its own
	/// [`Issued`] grants.
	pub fn authorize(&self, grant: &Grant) {
		let mut guard = self.state.lock();
		let state = &mut *guard;
		if state.limit.as_ref() == Some(grant) {
			return;
		}
		state.limit = Some(grant.clone());
		for link in state.links.values_mut() {
			link.epoch += 1;
			link.rescope(state.limit.as_ref());
		}
	}

	/// End the handle with `err`: every token ends, its sessions stop presenting them, and
	/// the peer's queued tokens are refused. A handle a session created for itself ends
	/// with that session.
	pub fn abort(&self, err: Error) {
		let queue = self.state.lock().abort(err);
		if let Some(queue) = queue {
			queue.close();
		}
	}

	/// Ask the acceptor to verify a token that rode on one of the peer's requests.
	///
	/// The token reaches it through [`requests`](Self::requests) like any other. The grant it
	/// answers covers only this request, never joins the session's scope, and ends when the
	/// returned [`RequestVerdict`] is dropped. Without an application acceptor, the verdict is
	/// [`Error::Unsupported`]. So it is for a token type other than
	/// [`Token::OUT_OF_BAND`](crate::setup::Token::OUT_OF_BAND), which an AUTH token cannot name,
	/// and for an empty token, which on AUTH means the connection's own credential and so would
	/// reach the acceptor as that credential. `hold` is the request's slot, kept while the
	/// application verifies the token.
	pub(crate) fn verify_request(&self, token: crate::setup::Token, hold: Option<Hold>) -> RequestVerdict {
		let Some(queue) = self.acceptor() else {
			return RequestVerdict { issue: None };
		};
		if token.kind != crate::setup::Token::OUT_OF_BAND || token.value.is_empty() {
			return RequestVerdict { issue: None };
		}
		let issue = kio::Shared::new(Issue {
			hold,
			..Default::default()
		});
		// A closed or full queue refuses the token.
		queue.push(Request::new(token.value.into(), issue.clone()));
		RequestVerdict { issue: Some(issue) }
	}

	/// Decide who answers the peer's tokens: the app if it took the requests,
	/// otherwise the session itself (`None`). Idempotent.
	fn acceptor(&self) -> Option<Pending> {
		let mut state = self.state.lock();
		match &state.acceptor {
			Acceptor::App(queue) => Some(queue.clone()),
			Acceptor::Default => None,
			Acceptor::Undecided => {
				state.acceptor = Acceptor::Default;
				None
			}
		}
	}
}

/// One session's side of a [`Handle`]: the tokens its driver presents, the peer's tokens
/// it answers, and the grants it enforces.
///
/// Cheap to clone; the session leaves its handle when the last clone drops.
#[derive(Clone)]
pub(crate) struct Link {
	handle: Handle,
	attached: std::sync::Arc<Detach>,
}

/// Removes a session from its handle once nothing holds its [`Link`].
struct Detach {
	state: kio::Shared<State>,
	id: u64,
}

impl Drop for Detach {
	fn drop(&mut self) {
		let queue = {
			let mut state = self.state.lock();
			// The link's credential may hold the answer a token set still waits on.
			state.record();
			state.links.remove(&self.id);
			// Dropped without closing, such as a driver dropped mid-session.
			match state.bound {
				true => state.abort(Error::Cancel),
				false => None,
			}
		};
		if let Some(queue) = queue {
			queue.close();
		}
	}
}

impl Link {
	/// A session that speaks AUTH (`supported`) or never will, on a handle of its own.
	pub(crate) fn new(supported: bool) -> Self {
		Self::bound(supported, false)
	}

	/// A moq-transport session: like [`new`](Self::new), but a request can carry a token
	/// itself, so a token needs no AUTH to be presented.
	pub(crate) fn ietf(supported: bool) -> Self {
		Self::bound(supported, true)
	}

	fn bound(supported: bool, requests: bool) -> Self {
		let handle = Handle {
			state: kio::Shared::new(State {
				bound: true,
				..Default::default()
			}),
		};
		Self::attach(&handle, supported, requests).expect("a fresh handle backs no session")
	}

	/// Build a session on `handle`, the moq-transport flavor when `requests`.
	///
	/// # Errors
	///
	/// [`Error::Duplicate`] while the handle backs another active session, or the
	/// handle's end.
	pub(crate) fn attach(handle: &Handle, supported: bool, requests: bool) -> Result<Self> {
		let mut guard = handle.state.lock();
		let state = &mut *guard;
		if let Some(err) = &state.closed {
			return Err(err.clone());
		}
		if state.links.values().any(Attached::active) {
			return Err(Error::Duplicate);
		}
		let id = state.next_link;
		state.next_link += 1;
		let mut link = Attached::new(supported, requests, state.limit.as_ref());
		let answers: Vec<_> = state
			.tokens
			.iter()
			.filter(|(_, token)| token.ended.is_none())
			.filter_map(|(token_id, token)| Some((*token_id, link.offer(*token_id, &token.value)?)))
			.collect();
		state.links.insert(id, link);
		for (token_id, answer) in answers {
			state.answer(token_id, answer.map(|()| Answer::Requests));
		}
		Ok(Self {
			handle: handle.clone(),
			attached: std::sync::Arc::new(Detach {
				state: handle.state.clone(),
				id,
			}),
		})
	}

	/// The handle this session is built on.
	pub(crate) fn handle(&self) -> &Handle {
		&self.handle
	}

	fn state(&self) -> &kio::Shared<State> {
		&self.handle.state
	}

	fn id(&self) -> u64 {
		self.attached.id
	}

	/// Run `f` on this session's side of the handle.
	fn with<R>(&self, f: impl FnOnce(&mut Attached) -> R) -> R {
		self.with_state(|state, id| f(attached(&mut state.links, id)))
	}

	/// Run `f` on the handle's state, with this session's id.
	fn with_state<R>(&self, f: impl FnOnce(&mut State, u64) -> R) -> R {
		f(&mut self.state().lock(), self.id())
	}

	/// Read this session's side of the handle, and the handle's state.
	fn read<R>(&self, f: impl FnOnce(&Attached, &State) -> R) -> R {
		let state = self.state().read();
		let link = state
			.links
			.get(&self.id())
			.expect("a link outlives its session's state");
		f(link, &state)
	}

	/// Retire the session to draining once `goaway` is sent or received, freeing the
	/// handle for a replacement.
	pub(crate) fn drain_with(&self, goaway: std::sync::Arc<crate::goaway::Handle>) {
		self.with(|link| link.goaway = Some(goaway));
	}

	/// Whether the session still takes new work: open, with no GOAWAY sent or received.
	pub(crate) fn active(&self) -> bool {
		self.read(|link, _| link.active())
	}

	/// Whether this side speaks AUTH: false on a version without it, on a side that does
	/// not offer it, and once the peer turns out not to.
	pub(crate) fn supported(&self) -> bool {
		self.read(|link, _| link.supported)
	}

	/// Present a token: the session's own credential at setup, presented on this session
	/// alone, or else one of the handle's, presented on every session.
	pub(crate) fn present(&self, token: Bytes, setup: bool) -> Result<Credential> {
		if !setup {
			return self.handle.present(token);
		}
		let id = self.with_state(|state, link| {
			let id = state.next_id;
			let link = attached(&mut state.links, link);
			if !link.supported && !link.requests {
				return Err(Error::Unsupported);
			}
			if let Some(err) = &link.closed {
				return Err(err.clone());
			}
			// Without AUTH only a request presents it, so nothing will answer.
			let answered = (!link.supported).then_some(Ok(()));
			link.slots.insert(id, Slot::new(token, true, answered));
			if link.supported {
				link.opening.push_back(id);
			}
			state.next_id += 1;
			Ok(id)
		})?;
		Ok(Credential {
			presented: std::sync::Arc::new(Presented {
				state: self.state().clone(),
				id,
				link: Some(self.id()),
			}),
		})
	}

	/// The token a request carries for `auth`, or `None` when the request goes out unnamed
	/// under the session's scope: `auth` came from another handle, it ended here, or
	/// requests carry no token on this version.
	pub(crate) fn token(&self, auth: &Credential) -> Option<Bytes> {
		if !auth.presented.state.same_channel(self.state()) {
			return None;
		}
		self.read(|link, _| {
			if !link.requests {
				return None;
			}
			let slot = link.slots.get(&auth.presented.id)?;
			slot.ended.is_none().then(|| slot.token.clone())
		})
	}

	/// Ask the acceptor to verify a token that rode on one of the peer's requests; see
	/// [`Handle::verify_request`].
	pub(crate) fn verify_request(&self, token: crate::setup::Token, hold: Option<Hold>) -> RequestVerdict {
		self.handle.verify_request(token, hold)
	}

	/// Decide who answers the peer's tokens: the app if it took the requests,
	/// otherwise the session itself (`None`). Idempotent.
	pub(crate) fn acceptor(&self) -> Option<Pending> {
		self.handle.acceptor()
	}

	/// The next token to open a stream for, or `Ready(None)` once the session ends.
	pub(crate) fn poll_opening(&self, waiter: &kio::Waiter) -> Poll<Option<(u64, Bytes)>> {
		let id = self.id();
		loop {
			let mut guard = ready_or!(self.state().poll(waiter, |state| {
				match state.links.get(&id) {
					Some(link) if link.opening.is_empty() && link.closed.is_none() => Poll::Pending,
					_ => Poll::Ready(()),
				}
			}));
			let Some(link) = guard.links.get_mut(&id) else {
				return Poll::Ready(None);
			};
			if link.closed.is_some() {
				return Poll::Ready(None);
			}
			let token = link.opening.pop_front().expect("checked non-empty");
			// Withdrawn or ended before its stream opened: nothing to present.
			if let Some(slot) = link.slots.get_mut(&token)
				&& slot.ended.is_none()
			{
				slot.streamed = true;
				slot.refused = false;
				return Poll::Ready(Some((token, slot.token.clone())));
			}
		}
	}

	/// Whether the token's stream should close: its handle dropped, or it ended, here or
	/// for good on another session.
	pub(crate) fn poll_withdrawn(&self, id: u64, waiter: &kio::Waiter) -> Poll<()> {
		let link = self.id();
		let _ = ready_or!(self.state().poll(waiter, |state| {
			match state.links.get(&link).and_then(|link| link.slots.get(&id)) {
				Some(slot) if !slot.withdrawn && slot.ended.is_none() => Poll::Pending,
				_ => Poll::Ready(()),
			}
		}));
		Poll::Ready(())
	}

	/// Record an AUTH_OK for the token.
	pub(crate) fn granted(&self, id: u64, grant: Grant) {
		// One parseable line per AUTH_OK, so the grant the peer actually sent is observable
		// without an API; the interop harness checks it against the token it minted.
		let list = |patterns: &Patterns| format!("{:?}", patterns.iter().map(|p| p.to_string()).collect::<Vec<_>>());
		tracing::debug!(
			publish = %list(&grant.publish),
			subscribe = %list(&grant.subscribe),
			"auth granted"
		);
		self.with_state(|state, link| {
			let link = attached(&mut state.links, link);
			let Some(slot) = link.slots.get_mut(&id) else {
				return;
			};
			if slot.ended.is_some() {
				return;
			}
			slot.grant = Some(grant);
			slot.answered = Some(Ok(()));
			slot.retry = None;
			link.replied = true;
			link.recompute();
			state.answer(id, Ok(Answer::Granted));
			// Latch a set this grant completed, before a later revocation can replace its answer.
			state.record();
		});
	}

	/// Record an AUTH_ERROR for the token: a reply that grants nothing, so a refused
	/// setup token leaves an empty union rather than an unknown (unrestricted) one.
	/// The driver then calls [`retry`](Self::retry), as on every end of the token's stream.
	pub(crate) fn refused(&self, id: u64) {
		self.with(|link| {
			if let Some(slot) = link.slots.get_mut(&id) {
				slot.refused = true;
				link.replied = true;
			}
		});
	}

	/// The delay before presenting a token again once its stream ended with `err`, or `None`
	/// when the driver should end it instead.
	///
	/// A token still wanted on an active session is presented again when the peer refused it
	/// for a reason that may pass, or, on a shared handle, when its stream was lost without a
	/// verdict, so the session never runs on without it. A session's own handle ends a lost
	/// token instead, which tells the application. Both back off from [`RETRY_INITIAL`],
	/// doubling until a grant resets it.
	pub(crate) fn retry(&self, id: u64, err: &Error) -> Option<Duration> {
		self.with_state(|state, link| {
			let bound = state.bound;
			let link = attached(&mut state.links, link);
			if !link.active() {
				return None;
			}
			let slot = link.slots.get_mut(&id)?;
			// A protocol violation ends the session anyway.
			if slot.withdrawn || slot.ended.is_some() || verdict(slot, err) || matches!(err, Error::ProtocolViolation) {
				return None;
			}
			if bound && !slot.refused {
				return None;
			}
			slot.refused = false;
			slot.streamed = false;
			slot.grant = None;
			// Unanswered again, as on its first presentation: a request or publication the
			// token might cover waits for the retry's verdict rather than being judged without it.
			slot.answered = None;
			let delay = slot.retry.map_or(RETRY_INITIAL, |delay| (delay * 2).min(RETRY_MAX));
			slot.retry = Some(delay);
			link.epoch += 1;
			link.recompute();
			Some(delay)
		})
	}

	/// The token to present again once a [`retry`](Self::retry) delay passes.
	///
	/// # Errors
	///
	/// [`Error::GoingAway`] once the session is draining, so a successor presents it instead
	/// and a handle of the session's own never reads it as the peer's clean close, or
	/// [`Error::Cancel`] when it is no longer wanted here.
	pub(crate) fn reopen(&self, id: u64) -> Result<Bytes> {
		self.with(|link| {
			if !link.active() {
				return Err(Error::GoingAway);
			}
			match link.slots.get_mut(&id) {
				Some(slot) if !slot.withdrawn && slot.ended.is_none() => {
					slot.streamed = true;
					Ok(slot.token.clone())
				}
				_ => Err(Error::Cancel),
			}
		})
	}

	/// End the token on this session, once [`retry`](Self::retry) declined to present it
	/// again: refused or revoked by the peer, withdrawn by us, or its stream lost while the
	/// session drains. A token that was never answered records `err` as its answer. The
	/// peer's verdict ends one of the handle's tokens on every session; anything else
	/// leaves it for the next session to present, except on a handle of the session's own.
	pub(crate) fn ended(&self, id: u64, err: Error) {
		self.with_state(|state, link_id| {
			// Latch a set already complete, so this end never replaces its answer.
			state.record();
			// A draining session's verdict is stale while an active session still presents the
			// token: that session's own answer decides, even mid-retry with no grant right now.
			let superseded = !attached(&mut state.links, link_id).active()
				&& state.links.iter().any(|(other, link)| {
					*other != link_id
						&& link.active() && link
						.slots
						.get(&id)
						.is_some_and(|slot| slot.ended.is_none() && !slot.withdrawn)
				});
			// A handle of the session's own has no next session to present it, so any end is final.
			let bound = state.bound;
			let link = attached(&mut state.links, link_id);
			let Some(slot) = link.slots.get_mut(&id) else {
				return;
			};
			let settled = !slot.setup
				&& !superseded
				&& link.closed.is_none()
				&& !slot.withdrawn
				&& slot.ended.is_none()
				&& (bound || verdict(slot, &err));
			slot.grant = None;
			slot.answered.get_or_insert(Err(err.clone()));
			slot.ended.get_or_insert(err.clone());
			// The handle is gone and the token is over: nothing will read the slot again.
			if slot.withdrawn {
				link.slots.remove(&id);
			}
			// Even with the union unchanged, a request carrying this token must drop it.
			link.epoch += 1;
			link.recompute();
			if settled {
				state.end_token(id, err);
			}
		});
	}

	/// Resolve once every token the session presented at setup has its first
	/// reply, so enforcement never waits on a token the app added later.
	pub(crate) fn poll_setup_answered(&self, waiter: &kio::Waiter) -> Poll<()> {
		let id = self.id();
		let _ = ready_or!(self.state().poll(waiter, |state| {
			let pending = state
				.links
				.get(&id)
				.is_some_and(|link| link.slots.values().any(|slot| slot.setup && slot.answered.is_none()));
			match pending {
				true => Poll::Pending,
				false => Poll::Ready(()),
			}
		}));
		Poll::Ready(())
	}

	/// Wait only when a token already presented might widen the grant for this path.
	fn poll_presented(&self, direction: Direction, path: &str, waiter: &kio::Waiter) -> Poll<()> {
		let id = self.id();
		let _ = ready_or!(self.state().poll(waiter, |state| {
			let Some(link) = state.links.get(&id) else {
				return Poll::Ready(());
			};
			let (granted, _) = link.parts(direction);
			if granted.is_some_and(|grant| grant.matches(path))
				|| !link
					.slots
					.values()
					.any(|slot| !slot.token.is_empty() && slot.answered.is_none() && !slot.withdrawn)
			{
				Poll::Ready(())
			} else {
				Poll::Pending
			}
		}));
		Poll::Ready(())
	}

	/// Record the grant an application issued to the peer's AUTH token `id`, or forget it with
	/// `None`, resizing the scope.
	fn issue(&self, id: u64, grant: Option<Grant>) {
		self.with_state(|state, link| {
			let link = attached(&mut state.links, link);
			match grant {
				Some(grant) => link.issued.insert(id, grant),
				None => link.issued.remove(&id),
			};
			link.rescope(state.limit.as_ref());
		});
	}

	/// The union once it changed since `epoch` last saw it, advancing `epoch`. It
	/// only ever changes into `Some`: no answer yet is the initial state.
	pub(crate) fn poll_union(&self, epoch: &mut u64, waiter: &kio::Waiter) -> Poll<Option<Grant>> {
		let state = ready_or!(self.poll_epoch(*epoch, waiter));
		let link = &state.links[&self.id()];
		*epoch = link.epoch;
		Poll::Ready(link.union.clone())
	}

	/// What `direction` allows once it may have changed since `epoch` last saw it,
	/// advancing `epoch`.
	pub(crate) fn poll_permit(&self, direction: Direction, epoch: &mut u64, waiter: &kio::Waiter) -> Poll<Permit> {
		let state = ready_or!(self.poll_epoch(*epoch, waiter));
		let link = &state.links[&self.id()];
		*epoch = link.epoch;
		Poll::Ready(link.permit(direction))
	}

	/// The limit once it may have changed since `epoch` last saw it, advancing `epoch`.
	pub(crate) fn poll_limit(&self, epoch: &mut u64, waiter: &kio::Waiter) -> Poll<Option<Grant>> {
		let state = ready_or!(self.poll_epoch(*epoch, waiter));
		*epoch = state.links[&self.id()].epoch;
		Poll::Ready(state.limit.clone())
	}

	fn poll_epoch(&self, seen: u64, waiter: &kio::Waiter) -> Poll<kio::Mut<'_, State>> {
		let id = self.id();
		self.state().poll(waiter, |state| match state.links[&id].epoch != seen {
			true => Poll::Ready(()),
			false => Poll::Pending,
		})
	}

	/// Whether `direction` allows `path` right now: the union (allowing everything
	/// while it is `None`, before an answer or on a version without AUTH) and the
	/// limit both.
	pub(crate) fn allows(&self, direction: Direction, path: &str) -> bool {
		self.read(|link, _| link.allows(direction, path))
	}

	/// Whether a request token decides the request carrying it, rather than the session's
	/// scope: the application verifies every token, or the request is refused as unsupported
	/// without an acceptor, even where the scope already covers it. A token admitted unread
	/// would leave the peer believing it backs a request this side holds to its scope. A side
	/// that neither scopes the peer nor answers tokens has nothing to check one against, and
	/// admits as it would without it.
	pub(crate) fn token_decides(&self, direction: Direction) -> bool {
		self.read(|link, state| {
			let (_, scope) = link.parts(direction);
			scope.is_some() || matches!(state.acceptor, Acceptor::App(_))
		})
	}

	/// Whether the peer's grant alone allows `direction` at `path`, allowing everything
	/// while it is `None`: what bounds this side whatever authorized the request.
	pub(crate) fn within_grant(&self, direction: Direction, path: &str) -> bool {
		self.read(|link, _| {
			let (granted, _) = link.parts(direction);
			granted.is_none_or(|granted| granted.matches(path))
		})
	}

	/// Whether the scope alone allows `direction` at `path`: for a route the peer
	/// offers us, which our own grant does not decide.
	pub(crate) fn within_limit(&self, direction: Direction, path: &str) -> bool {
		self.read(|link, _| {
			let (_, scope) = link.parts(direction);
			scope.is_none_or(|scope| scope.matches(path))
		})
	}

	/// The peer turned out not to negotiate AUTH, leaving the union unknown. On
	/// moq-transport every token we presented stays, with an unknown grant, for the
	/// requests that name it; elsewhere each fails as unsupported, which ends the handle's
	/// tokens for good. The peer's tokens can still ride its requests, so their queue
	/// stays open.
	pub(crate) fn unsupported(&self) {
		self.with_state(|state, link| {
			let link = attached(&mut state.links, link);
			link.supported = false;
			link.opening.clear();
			let requests = link.requests;
			let mut answers = Vec::new();
			for (id, slot) in &mut link.slots {
				if slot.withdrawn {
					continue;
				}
				let answer = match requests {
					true => Ok(()),
					false => Err(Error::Unsupported),
				};
				slot.answered.get_or_insert(answer.clone());
				if let Err(err) = &answer {
					slot.ended.get_or_insert(err.clone());
				}
				if !slot.setup {
					answers.push((*id, answer));
				}
			}
			// Nothing will read a withdrawn slot again.
			link.slots.retain(|_, slot| !slot.withdrawn);
			for (id, answer) in answers {
				state.answer(id, answer.map(|()| Answer::Requests));
			}
		});
	}

	/// Why the session ended, once it has.
	fn closed(&self) -> Option<Error> {
		self.read(|link, _| link.closed.clone())
	}

	/// The peer accepts the connection credential only with its SETUP, which this client
	/// reads after the session is live, while the server can still refuse it.
	pub(crate) fn await_setup(&self) {
		self.with(|link| link.accepted = false);
	}

	/// The peer's SETUP arrived, so it accepted the connection credential.
	pub(crate) fn accepted(&self) {
		self.with_state(|state, link| {
			attached(&mut state.links, link).accepted = true;
			// Latch a set this acceptance completed, before a later end can replace its answer.
			state.record();
		});
	}

	/// End the session: fail every pending token here, leaving the handle's tokens for the
	/// next session. A handle the session created for itself ends with it.
	pub(crate) fn close(&self, err: Error) {
		let queue = self.with_state(|state, link| {
			if state.links[&link].closed.is_some() {
				return None;
			}
			// The session's credential may hold the answer a token set still waits on.
			state.record();
			let link = attached(&mut state.links, link);
			link.closed = Some(err.clone());
			link.opening.clear();
			for slot in link.slots.values_mut() {
				slot.grant = None;
				slot.answered.get_or_insert(Err(err.clone()));
				slot.ended.get_or_insert(err.clone());
			}
			link.recompute();
			match state.bound {
				true => state.abort(err),
				false => None,
			}
		});
		// Refusing the queued tokens takes the lock again, to drop them from the scope.
		if let Some(queue) = queue {
			queue.close();
		}
	}
}

/// A session's side of the handle, which lives as long as any [`Link`] to it.
fn attached(links: &mut BTreeMap<u64, Attached>, id: u64) -> &mut Attached {
	links.get_mut(&id).expect("a link outlives its session's state")
}

/// One credential this side presented, returned by [`Handle`]'s `add`.
///
/// Cheap to clone, and every clone names the same credential. It stays presented on
/// every session of its handle until the last clone drops, which closes its streams and
/// removes its grant from the union. A [`Route`](crate::origin::Route) holding one keeps
/// it presented while the route stands, across the handle's sessions.
#[derive(Clone)]
pub struct Credential {
	presented: std::sync::Arc<Presented>,
}

#[cfg(test)]
impl Credential {
	/// The id the session presents this credential under.
	pub(crate) fn id(&self) -> u64 {
		self.presented.id
	}
}

/// The token every clone of one [`Credential`] shares, withdrawn when the last drops.
struct Presented {
	state: kio::Shared<State>,
	id: u64,
	/// The one session that presents it: a session's own credential. `None` for a
	/// handle's token, presented on every session.
	link: Option<u64>,
}

impl Presented {
	fn selector(&self) -> Selector {
		match self.link {
			Some(link) => Selector::Setup { link, id: self.id },
			None => Selector::Token(self.id),
		}
	}
}

impl Credential {
	/// This credential's own grant on the live session: `None` until its peer answers,
	/// while no session is live, forever on moq-transport without AUTH, and again once
	/// the credential ends.
	pub fn grant(&self) -> Watch {
		Watch::new(self.presented.state.clone(), self.presented.selector())
	}

	/// Wait for the credential to end for good: a peer's refusal or revocation as
	/// [`Error::Session`], [`Error::Cancel`] when the peer closed it without one, or the
	/// handle's end. A session ending is not one: the next session presents it again.
	#[cfg(feature = "async")]
	pub async fn closed(&self) -> Error {
		let selector = self.presented.selector();
		kio::wait(|waiter| {
			let state = ready_or!(self.presented.state.poll(waiter, |state| {
				match selector.ended(state) {
					Some(_) => Poll::Ready(()),
					None => Poll::Pending,
				}
			}));
			Poll::Ready(selector.ended(&state).unwrap_or(Error::Cancel))
		})
		.await
	}

	/// Poll for the first verdict on a handle's token: a grant on any session, or the
	/// refusal or end that settles it.
	fn poll_answered(&self, waiter: &kio::Waiter) -> Poll<Result<Answer>> {
		let id = self.presented.id;
		let answered = |state: &State| match state.tokens.get(&id) {
			Some(token) => token.answered.clone(),
			None => Some(Err(Error::Cancel)),
		};
		let state = ready_or!(self.presented.state.poll(waiter, |state| match answered(state) {
			Some(_) => Poll::Ready(()),
			None => Poll::Pending,
		}));
		Poll::Ready(answered(&state).unwrap_or(Err(Error::Cancel)))
	}
}

impl PartialEq for Credential {
	fn eq(&self, other: &Self) -> bool {
		std::sync::Arc::ptr_eq(&self.presented, &other.presented)
	}
}

impl Eq for Credential {}

impl std::fmt::Debug for Credential {
	// Never the token: routes are logged, and a credential must not be.
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		f.debug_tuple("Credential").field(&self.presented.id).finish()
	}
}

impl Drop for Presented {
	fn drop(&mut self) {
		let mut guard = self.state.lock();
		let state = &mut *guard;
		match self.link {
			Some(link) => {
				if let Some(link) = state.links.get_mut(&link) {
					link.withdraw(self.id);
				}
			}
			None => state.withdraw(self.id),
		}
	}
}

/// Which grant a [`Watch`] follows.
#[derive(Clone, Copy)]
enum Selector {
	/// The live session's union.
	Union,
	/// A handle's token, on the live session.
	Token(u64),
	/// A session's own credential, on that session.
	Setup { link: u64, id: u64 },
}

impl Selector {
	fn current<'a>(&self, state: &'a State) -> Option<&'a Grant> {
		match *self {
			Self::Union => state.live()?.union.as_ref(),
			Self::Token(id) => state.live()?.slots.get(&id)?.grant.as_ref(),
			Self::Setup { link, id } => state.links.get(&link)?.slots.get(&id)?.grant.as_ref(),
		}
	}

	/// Why nothing will change the grant again, once nothing will.
	fn ended(&self, state: &State) -> Option<Error> {
		if let Some(err) = &state.closed {
			return Some(err.clone());
		}
		match *self {
			Self::Union => None,
			Self::Token(id) => match state.tokens.get(&id) {
				Some(token) => token.ended.clone(),
				None => Some(Error::Cancel),
			},
			Self::Setup { link, id } => match state.links.get(&link).and_then(|link| link.slots.get(&id)) {
				Some(slot) => slot.ended.clone(),
				None => Some(Error::Cancel),
			},
		}
	}
}

/// Watches a grant: the live session's union, or one credential's.
///
/// Created by [`Handle::grant`] and [`Credential::grant`].
pub struct Watch {
	state: kio::Shared<State>,
	selector: Selector,
	last: Option<Grant>,
}

impl Watch {
	fn new(state: kio::Shared<State>, selector: Selector) -> Self {
		let mut watch = Self {
			state,
			selector,
			last: None,
		};
		watch.last = watch.peek();
		watch
	}

	/// The grant right now.
	pub fn peek(&self) -> Option<Grant> {
		self.selector.current(&self.state.read()).cloned()
	}

	/// Poll for the grant to differ from the last one this watch returned (or saw
	/// at creation).
	///
	/// `Err` once nothing will change it again: the handle ended, or for a
	/// token's watch, the token ended for good.
	pub fn poll_changed(&mut self, waiter: &kio::Waiter) -> Poll<Result<Option<Grant>>> {
		let last = &self.last;
		let selector = self.selector;
		let state = ready_or!(self.state.poll(waiter, |state| {
			match selector.current(state) != last.as_ref() || selector.ended(state).is_some() {
				true => Poll::Ready(()),
				false => Poll::Pending,
			}
		}));
		let current = selector.current(&state).cloned();
		if current != self.last {
			self.last = current.clone();
			return Poll::Ready(Ok(current));
		}
		Poll::Ready(Err(selector.ended(&state).unwrap_or(Error::Cancel)))
	}

	/// Wait for the grant to change, returning the new one.
	///
	/// # Errors
	///
	/// See [`poll_changed`](Self::poll_changed).
	#[cfg(feature = "async")]
	pub async fn changed(&mut self) -> Result<Option<Grant>> {
		kio::wait(|waiter| self.poll_changed(waiter)).await
	}
}

/// The tokens the peer presents, for an application that answers them itself.
///
/// Returned by `server::Handshake::requests`. Dropping it refuses every later token.
pub struct Requests {
	queue: Pending,
}

impl Requests {
	/// Poll for the next token, or `None` once the handle ends.
	pub fn poll_next(&mut self, waiter: &kio::Waiter) -> Poll<Option<Request>> {
		self.queue.poll_next(waiter)
	}

	/// Wait for the next token the peer presents, or `None` once the handle ends.
	#[cfg(feature = "async")]
	pub async fn next(&mut self) -> Option<Request> {
		kio::wait(|waiter| self.poll_next(waiter)).await
	}
}

impl Drop for Requests {
	fn drop(&mut self) {
		self.queue.close();
	}
}

/// Pending application decisions, bounded by the stream and request credit each one holds.
#[derive(Clone, Default)]
pub(crate) struct Pending {
	state: kio::Shared<PendingState>,
}

#[derive(Default)]
struct PendingState {
	requests: VecDeque<Request>,
	/// How many requests survived the last prune: pruning again only once the queue doubles
	/// keeps a push amortized O(1).
	survivors: usize,
	closed: bool,
}

impl Pending {
	fn close(&self) {
		let mut state = self.state.lock();
		state.closed = true;
		state.requests.clear();
	}

	/// Queue the token for the application, or refuse it when closed.
	pub(crate) fn push(&self, request: Request) {
		let mut state = self.state.lock();
		// Dropping the request refuses it as unanswered.
		if state.closed {
			return;
		}
		// A withdrawn token gave its credit back, so it must not stay queued while the
		// acceptor is busy: that would let a peer that keeps withdrawing grow the queue.
		if state.requests.len() >= 2 * state.survivors.max(1) {
			state.requests.retain(|request| request.issue().read().peer.is_none());
			state.survivors = state.requests.len();
		}
		state.requests.push_back(request);
	}

	fn poll_next(&self, waiter: &kio::Waiter) -> Poll<Option<Request>> {
		loop {
			let mut state = std::task::ready!(self.state.poll(waiter, |state| {
				if state.closed || !state.requests.is_empty() {
					Poll::Ready(())
				} else {
					Poll::Pending
				}
			}));
			while let Some(request) = state.requests.pop_front() {
				// Taken under the same lock its withdrawal checks, so from here the application
				// owns the request's slot until it answers.
				let mut issue = request.issue().lock();
				if issue.peer.is_none() {
					issue.taken = true;
					drop(issue);
					return Poll::Ready(Some(request));
				}
			}
			if state.closed {
				return Poll::Ready(None);
			}
		}
	}
}

/// What the acceptor writes on one peer token's stream.
pub(crate) enum Reply {
	Grant(Grant),
	Refuse { code: SessionError, reason: String },
}

/// The acceptor's side of one peer token's stream, shared with the driver.
#[derive(Default)]
pub(crate) struct Issue {
	/// Replies for the driver to write, in order.
	pub outbox: VecDeque<Reply>,
	/// The app is done with the stream: the driver closes it once the outbox drains.
	pub done: bool,
	/// Why the stream ended on the peer's side, once it has.
	pub peer: Option<Error>,
	/// A request token's share of its request's slot, held while the application verifies it.
	hold: Option<Hold>,
	/// The application took the token off the queue.
	taken: bool,
	/// An AUTH token's place in the session's scope, which its grant joins. A token that rode
	/// on a request has none: its grant covers only that request.
	scoped: Option<(Link, u64)>,
}

impl Issue {
	/// Push a reply, keeping the session's scope in step with the grant an AUTH token holds.
	/// Called under the issue's lock, so a withdrawal recorded under it is never undone.
	fn reply(&mut self, reply: Reply) {
		if let Some((handle, id)) = &self.scoped {
			match &reply {
				Reply::Grant(grant) if self.peer.is_none() => handle.issue(*id, Some(grant.clone())),
				_ => handle.issue(*id, None),
			}
		}
		self.outbox.push_back(reply);
	}

	/// The application is done with the token: its grant leaves the scope.
	fn finish(&mut self) {
		self.done = true;
		if let Some((handle, id)) = &self.scoped {
			handle.issue(*id, None);
		}
	}
}

/// A request's slot in the session's count, shared with the application's verification of its
/// token so work still in flight stays counted after the request ends. Opaque here: auth only
/// keeps it alive.
pub(crate) type Hold = std::sync::Arc<dyn std::any::Any + Send + Sync>;

/// The serving task's hold on one peer token's [`Issue`].
///
/// Dropping it settles [`Issued::closed`] with the session's error, unless the task
/// already recorded why the stream ended, so no exit path leaves the acceptor waiting.
pub(crate) struct Serving {
	pub issue: kio::Shared<Issue>,
	handle: Link,
}

impl Serving {
	/// A fresh stream's shared state, settled by the session `handle` if the task drops.
	pub(crate) fn new(handle: Link) -> Self {
		let id = {
			let mut state = handle.state().lock();
			state.next_issue += 1;
			state.next_issue
		};
		Self {
			issue: kio::Shared::new(Issue {
				scoped: Some((handle.clone(), id)),
				..Default::default()
			}),
			handle,
		}
	}

	/// Record why the stream ended on the peer's side; the first reason wins. Whatever it
	/// granted leaves the scope.
	pub(crate) fn end(&self, err: Error) {
		let mut issue = self.issue.lock();
		issue.peer.get_or_insert(err);
		if let Some((handle, id)) = &issue.scoped {
			handle.issue(*id, None);
		}
	}
}

impl Drop for Serving {
	fn drop(&mut self) {
		let err = self.handle.closed().unwrap_or(Error::Cancel);
		self.end(err);
	}
}

/// A token the peer presented, waiting for an answer.
///
/// It arrived on its own AUTH stream, or on one of the peer's moq-transport requests. Either
/// way the grant it earns is enforced by the session: an AUTH token's joins the session's
/// scope, while one that rode on a request admits only that request, which ends with it.
///
/// Dropping it unanswered refuses the token with [`SessionError::Unauthorized`].
pub struct Request {
	token: Bytes,
	issue: Option<kio::Shared<Issue>>,
}

impl Request {
	pub(crate) fn new(token: Bytes, issue: kio::Shared<Issue>) -> Self {
		Self {
			token,
			issue: Some(issue),
		}
	}

	fn issue(&self) -> &kio::Shared<Issue> {
		self.issue.as_ref().expect("unanswered request")
	}

	/// The token the peer presented. Empty means the credential its connection
	/// already carried, or none.
	pub fn token(&self) -> &Bytes {
		&self.token
	}

	/// Wait for the peer to withdraw this unanswered token, end the request it rode on, or
	/// close the session.
	#[cfg(feature = "async")]
	pub async fn closed(&self) -> Error {
		closed(self.issue()).await
	}

	/// Grant the token. The grant holds until the returned [`Issued`] is revoked
	/// or dropped, or the peer withdraws the token or ends the request it rode on.
	///
	/// Revoking an AUTH token's grant once `expires` passes is the acceptor's job; a grant
	/// on a request lapses at `expires` by itself.
	pub fn accept(mut self, grant: Grant) -> Issued {
		let issue = self.issue.take().expect("answered once");
		let mut locked = issue.lock();
		locked.reply(Reply::Grant(grant));
		locked.hold = None;
		drop(locked);
		Issued { issue }
	}

	/// Refuse the token with a session error code and a reason for the peer.
	pub fn reject(mut self, code: SessionError, reason: impl Into<String>) {
		let issue = self.issue.take().expect("answered once");
		refuse(&issue, code, reason.into());
		issue.lock().hold = None;
	}
}

impl Drop for Request {
	fn drop(&mut self) {
		if let Some(issue) = self.issue.take() {
			refuse(&issue, SessionError::Unauthorized, "unanswered".to_string());
			issue.lock().hold = None;
		}
	}
}

fn refuse(issue: &kio::Shared<Issue>, code: SessionError, reason: String) {
	let mut issue = issue.lock();
	if !issue.done {
		issue.reply(Reply::Refuse { code, reason });
		issue.finish();
	}
}

/// Which request a token rode on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RequestKind {
	/// A SUBSCRIBE: the peer reads the named track.
	Subscribe,
	/// A PUBLISH_NAMESPACE: the peer announces a namespace it will publish under.
	PublishNamespace,
}

impl RequestKind {
	/// Whether `grant`, issued to the peer that presented the token, covers this request at
	/// `path`. A grant names what its holder may do, so a request to read is covered by its
	/// `subscribe` patterns and a request to announce by its `publish` patterns. An announce
	/// routes everything beneath its namespace, so the grant must cover that whole subtree.
	pub(crate) fn covers(self, grant: &Grant, path: &str) -> bool {
		match self {
			Self::Subscribe => grant.subscribe.matches(path),
			Self::PublishNamespace => {
				crate::Pattern::subtree(path).is_ok_and(|subtree| grant.publish.contains(&subtree))
			}
		}
	}
}

/// The verdict on a request-borne token, from [`Link::verify_request`].
///
/// Holding it keeps the request's grant alive; dropping it tells the acceptor the request
/// is over, so the grant lives exactly as long as the request and never outlives it.
pub(crate) struct RequestVerdict {
	issue: Option<kio::Shared<Issue>>,
}

/// Why a request token did not authorize its request: the error whose code the peer is
/// sent, and the reason phrase with it, which the acceptor chose when it refused.
#[derive(Debug, Clone)]
pub(crate) struct Refusal {
	pub error: Error,
	pub reason: String,
}

impl Refusal {
	fn new(error: Error, reason: &str) -> Self {
		Self {
			error,
			reason: reason.to_string(),
		}
	}
}

impl From<Error> for Refusal {
	fn from(error: Error) -> Self {
		let reason = error.to_string();
		Self { error, reason }
	}
}

impl RequestVerdict {
	/// Wait for the acceptor's first answer. See [`poll_grant`](Self::poll_grant).
	#[cfg(test)]
	pub(crate) async fn grant(&self) -> std::result::Result<Grant, Refusal> {
		kio::wait(|waiter| self.poll_grant(waiter)).await
	}

	/// Poll for the acceptor's first answer: the grant it issued, or its refusal, which the
	/// caller sends the peer. No consumer is [`Error::Unsupported`]; an unanswered (dropped)
	/// request is [`SessionError::Unauthorized`]. Whatever the acceptor did since accepting
	/// is folded in, so a grant already updated, revoked or dropped is never admitted as it
	/// first stood.
	pub(crate) fn poll_grant(&self, waiter: &kio::Waiter) -> Poll<std::result::Result<Grant, Refusal>> {
		let Some(issue) = &self.issue else {
			return Poll::Ready(Err(Error::Unsupported.into()));
		};
		let mut guard = ready_or!(
			issue.poll(waiter, |issue| match issue.outbox.is_empty() && !issue.done {
				true => Poll::Pending,
				false => Poll::Ready(()),
			})
		);
		let mut grant = None;
		while let Some(reply) = guard.outbox.pop_front() {
			match reply {
				Reply::Grant(next) => grant = Some(next),
				Reply::Refuse { code, reason } => return Poll::Ready(Err(Refusal::new(Error::Session(code), &reason))),
			}
		}
		Poll::Ready(match (grant, guard.done) {
			(Some(grant), false) => Ok(grant),
			(Some(_), true) => Err(Refusal::new(
				Error::Session(SessionError::Unauthorized),
				"grant dropped",
			)),
			// Done with nothing written: the acceptor dropped the request unanswered.
			(None, _) => Err(Error::Session(SessionError::Unauthorized).into()),
		})
	}

	/// After the first grant, poll for the acceptor's next action on this token: a
	/// replacement grant (an update, such as a lowered expiry), a refusal (revoke), or the
	/// enforced grant being dropped (`None`). `Pending` while the grant still stands.
	fn poll_reply(&self, waiter: &kio::Waiter) -> Poll<Option<Reply>> {
		let Some(issue) = &self.issue else {
			return Poll::Ready(Some(Reply::Refuse {
				code: SessionError::Unauthorized,
				reason: "no verifier".to_string(),
			}));
		};
		let mut guard = ready_or!(
			issue.poll(waiter, |issue| match issue.outbox.is_empty() && !issue.done {
				true => Poll::Pending,
				false => Poll::Ready(()),
			})
		);
		Poll::Ready(guard.outbox.pop_front())
	}
}

impl Drop for RequestVerdict {
	fn drop(&mut self) {
		if let Some(issue) = &self.issue {
			let mut issue = issue.lock();
			// The request is over: settle the acceptor's `closed`, and make a late answer land
			// nowhere. A token still queued gives its slot back now; one the application took
			// keeps it until the application answers.
			issue.peer.get_or_insert(Error::Cancel);
			if !issue.taken {
				issue.hold = None;
			}
		}
	}
}

/// A request-borne grant, held for the life of the request it authorized (a SUBSCRIBE or a
/// PUBLISH_NAMESPACE). It carries the acceptor's grant and a deadline armed at the grant's
/// expiry, and ends the request when the deadline lapses or the acceptor revokes or drops the
/// grant. A REQUEST_UPDATE the acceptor accepts [`renew`](Self::renew)s it with a fresh grant
/// and expiry; one it refuses is not applied here, and the serve loop ends the request.
///
/// Clones share the grant: the request's [`until`] watches it end while the request renews it.
/// Ending a request grant never touches the session: only that one request ends.
#[derive(Clone)]
pub(crate) struct RequestGrant(std::sync::Arc<crate::sync::Mutex<Held>>);

struct Held {
	verdict: RequestVerdict,
	grant: Grant,
	deadline: crate::time::Deadline,
	/// The request this grant must keep covering: a replacement that no longer does ends it.
	path: crate::Path,
	kind: RequestKind,
	/// The request's slot, shared with a renewal's verification.
	hold: Option<Hold>,
	/// The token of the renewal being verified, until the serve loop applies its verdict.
	renewing: Option<kio::Shared<Issue>>,
	clock: crate::time::Deadlines,
	/// Why the request ended, once it has. A channel, so every clone waiting on the grant
	/// wakes when [`RequestGrant::end`] latches it.
	ended: kio::Producer<Option<Refusal>>,
}

impl Held {
	/// See [`RequestGrant::poll_ended`].
	fn poll_end(&mut self, waiter: &kio::Waiter) -> Poll<Refusal> {
		loop {
			// The acceptor granted a renewal the serve loop has yet to apply: it takes over rather
			// than the request ending between the two, whether the old grant lapsed or dropped.
			// Watched, so a request that abandons the renewal, such as one cancelled while a
			// joined FETCH outlives it, falls back to the old grant's end.
			let handover = self.renewing.as_ref().is_some_and(|issue| {
				issue
					.poll(waiter, |issue| match granted(issue) {
						true => Poll::Pending,
						false => Poll::Ready(()),
					})
					.is_pending()
			});
			if self.deadline.poll(waiter).is_ready() {
				if handover {
					return Poll::Pending;
				}
				return Poll::Ready(Refusal::new(Error::Expired, "request token expired"));
			}
			match self.verdict.poll_reply(waiter) {
				Poll::Ready(None) if handover => return Poll::Pending,
				Poll::Ready(None) => {
					return Poll::Ready(Refusal::new(
						Error::Session(SessionError::Unauthorized),
						"grant dropped",
					));
				}
				Poll::Ready(Some(Reply::Grant(grant))) => {
					if !self.kind.covers(&grant, self.path.as_str()) {
						return Poll::Ready(Refusal::new(Error::Unauthorized, "request token no longer granted"));
					}
					self.deadline.set(grant.expires);
					self.grant = grant;
					// Re-poll: the new deadline may already have lapsed, or another reply may
					// be waiting.
					continue;
				}
				Poll::Ready(Some(Reply::Refuse { code, reason })) => {
					return Poll::Ready(Refusal::new(Error::Session(code), &reason));
				}
				Poll::Pending => return Poll::Pending,
			}
		}
	}
}

impl RequestGrant {
	/// Hold the acceptor's first answer `res` for the request's life, armed to lapse at its
	/// expiry, or the refusal to answer the request with: the acceptor's own, or
	/// [`Error::Unauthorized`] for a grant that does not cover it or has already expired.
	pub(crate) fn new(
		clock: &crate::time::Deadlines,
		verdict: RequestVerdict,
		res: std::result::Result<Grant, Refusal>,
		path: crate::Path,
		kind: RequestKind,
		hold: Option<Hold>,
	) -> std::result::Result<Self, Refusal> {
		let grant = res?;
		if !live(clock, kind, path.as_str(), &grant) {
			return Err(Refusal::new(Error::Unauthorized, "request token not granted"));
		}
		let mut deadline = crate::time::Deadline::new(clock);
		deadline.set(grant.expires);
		Ok(Self(std::sync::Arc::new(crate::sync::Mutex::new(Held {
			verdict,
			grant,
			deadline,
			path,
			kind,
			hold,
			renewing: None,
			clock: clock.clone(),
			ended: kio::Producer::new(None),
		}))))
	}

	fn held(&self) -> crate::sync::MutexGuard<'_, Held> {
		self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
	}

	/// The grant in force right now. Observed by the lifecycle tests.
	#[cfg(test)]
	pub(crate) fn grant(&self) -> Grant {
		self.held().grant.clone()
	}

	/// The request's slot, for re-attaching what it authorized without taking another.
	pub(crate) fn hold(&self) -> Option<Hold> {
		self.held().hold.clone()
	}

	/// Start verifying a renewal's token, holding the request's slot while it runs.
	pub(crate) fn verify_renewal(&self, handle: &Link, token: crate::setup::Token) -> RequestVerdict {
		let mut held = self.held();
		let verdict = handle.verify_request(token, held.hold.clone());
		held.renewing = verdict.issue.clone();
		verdict
	}

	/// Apply a renewal's verdict, or return the refusal that ends the request.
	///
	/// A live grant that still covers the request replaces this one: the new verdict is
	/// adopted (dropping the old, which ends the old token) and the deadline re-armed at the
	/// new expiry. A refusal, or a grant that no longer covers the request or has already
	/// expired, changes nothing here; the caller ends only that request.
	pub(crate) fn renew(
		&self,
		verdict: RequestVerdict,
		res: std::result::Result<Grant, Refusal>,
	) -> std::result::Result<(), Refusal> {
		let mut held = self.held();
		held.renewing = None;
		let grant = res?;
		if !live(&held.clock, held.kind, held.path.as_str(), &grant) {
			return Err(Refusal::new(Error::Unauthorized, "renewal not granted"));
		}
		held.verdict = verdict;
		held.deadline.set(grant.expires);
		held.grant = grant;
		Ok(())
	}

	/// End the grant with `refusal`, waking every clone still watching it, unless it already
	/// ended. A refused renewal ends the request without the grant ending itself, so the serve
	/// loop calls this to end anything else serving under the grant, such as a joining FETCH
	/// that outlives its subscription.
	pub(crate) fn end(&self, refusal: Refusal) {
		latch(&self.held().ended, refusal);
	}

	/// Resolve with the refusal that ends the request: the deadline lapsing
	/// ([`Error::Expired`]), the acceptor revoking or dropping the grant (its code and
	/// reason), or an acceptor-side update that no longer covers the request
	/// ([`Error::Unauthorized`]). A covering update, such as a lowered expiry, is folded in and
	/// polling continues. Never resolves while the grant still stands.
	pub(crate) fn poll_ended(&self, waiter: &kio::Waiter) -> Poll<Refusal> {
		let held = &mut *self.held();
		// Latched, as each acceptor reply is read once: every clone watching the grant sees
		// the same end, whichever polled first.
		let latched = match held.ended.poll_ref(waiter, |ended| match &**ended {
			Some(ended) => Poll::Ready(ended.clone()),
			None => Poll::Pending,
		}) {
			Poll::Ready(Ok(ended)) => Some(ended),
			// Never closed: the grant owns its only producer.
			Poll::Ready(Err(_)) | Poll::Pending => None,
		};
		if let Some(ended) = latched {
			return Poll::Ready(ended);
		}
		let ended = std::task::ready!(held.poll_end(waiter));
		latch(&held.ended, ended.clone());
		Poll::Ready(ended)
	}

	/// Run `body`, everything a request does once admitted, until it finishes or `grant`
	/// (when its token admitted it) ends. See [`until`].
	pub(crate) fn guard<F: std::future::Future + Send>(
		grant: Option<&Self>,
		body: F,
	) -> impl std::future::Future<Output = std::result::Result<F::Output, Refusal>> + Send {
		crate::util::sendable(until(body, move |waiter| match grant {
			Some(grant) => grant.poll_ended(waiter),
			None => Poll::Pending,
		}))
	}
}

/// Record `refusal` as why a request grant ended, unless an earlier end already is.
fn latch(ended: &kio::Producer<Option<Refusal>>, refusal: Refusal) {
	if let Ok(mut ended) = ended.write()
		&& ended.is_none()
	{
		*ended = Some(refusal);
	}
}

/// Run `body` until it finishes, or until `ended` resolves first, which drops `body` wherever
/// it waits.
///
/// An admitted request runs everything it does under this, its setup included, so authority
/// ending at any await ends the request there, rather than only where a loop remembers to
/// check. `ended` is polled first, so a request never acts on authority that already ended.
/// A dropped `body` releases what it holds; anything it was writing stays buffered whole in
/// the stream's [`Writer`](crate::coding::Writer) as long as each message is buffered before
/// its first await, so the caller can still end the request on the wire.
pub(crate) async fn until<F: std::future::Future>(
	body: F,
	mut ended: impl FnMut(&kio::Waiter) -> Poll<Refusal>,
) -> std::result::Result<F::Output, Refusal> {
	let mut body = std::pin::pin!(body);
	kio::wait(|waiter| {
		if let Poll::Ready(refusal) = ended(waiter) {
			return Poll::Ready(Err(refusal));
		}
		waiter.poll_future(body.as_mut()).map(Ok)
	})
	.await
}

/// Whether `grant` covers a `kind` request at `path` and has not already expired: admitting
/// an expired one until its deadline fires would let others act on it meanwhile.
fn live(clock: &crate::time::Deadlines, kind: RequestKind, path: &str, grant: &Grant) -> bool {
	kind.covers(grant, path) && grant.expires.is_none_or(|expires| expires > clock.now())
}

/// Whether the acceptor granted the token behind `issue` and still holds the grant, for a
/// request still waiting to apply it.
fn granted(issue: &Issue) -> bool {
	issue.peer.is_none() && !issue.done && issue.outbox.iter().any(|reply| matches!(reply, Reply::Grant(_)))
}

/// A grant issued to one of the peer's tokens, which the session enforces. Dropping it
/// ends the grant: an AUTH token's leaves the scope, and a request's token ends the request.
pub struct Issued {
	issue: kio::Shared<Issue>,
}

impl Issued {
	/// Replace the grant, such as with a lowered expiry. A request's token whose new grant
	/// no longer covers the request ends it.
	pub fn update(&self, grant: Grant) {
		let mut issue = self.issue.lock();
		if !issue.done {
			issue.reply(Reply::Grant(grant));
		}
	}

	/// Revoke the grant with a session error code and a reason for the peer.
	pub fn revoke(self, code: SessionError, reason: impl Into<String>) {
		refuse(&self.issue, code, reason.into());
	}

	/// Wait for the peer to withdraw the token ([`Error::Cancel`]), the request it rode on
	/// to end ([`Error::Cancel`]), or its stream or the session to end.
	#[cfg(feature = "async")]
	pub async fn closed(&self) -> Error {
		closed(&self.issue).await
	}
}

impl Drop for Issued {
	fn drop(&mut self) {
		self.issue.lock().finish();
	}
}

#[cfg(feature = "async")]
async fn closed(issue: &kio::Shared<Issue>) -> Error {
	kio::wait(|waiter| {
		let issue = ready_or!(issue.poll(waiter, |issue| {
			if issue.peer.is_some() {
				Poll::Ready(())
			} else {
				Poll::Pending
			}
		}));
		Poll::Ready(issue.peer.clone().expect("checked above"))
	})
	.await
}

/// Which way media flows, from this side's view.
#[derive(Clone, Copy)]
pub(crate) enum Direction {
	/// This side sends to the peer: our grant's `publish`, and the limit's `subscribe`.
	Publish,
	/// The peer sends to this side: our grant's `subscribe`, and the limit's `publish`.
	Subscribe,
}

/// Watches whether the session still allows one path, for a request that must end
/// once it does not.
///
/// Create it at the request's first check and hold it to the end, so a change
/// landing in between is never missed. Cheap to poll on every wakeup: the path is
/// only re-matched when the union or the limit changes.
pub(crate) struct Gate {
	handle: Link,
	path: crate::Path,
	direction: Direction,
	epoch: u64,
	part: Part,
	/// Whether the scope allowed the path at the last permit.
	within_limit: bool,
	admitted: bool,
	waiting: Option<Waiting>,
}

/// Log the whole wait once, including when its request is cancelled.
struct Waiting {
	path: crate::Path,
	started: Instant,
}

impl Drop for Waiting {
	fn drop(&mut self) {
		tracing::debug!(path = %self.path, waited = ?self.started.elapsed(), "request waited for authorization");
	}
}

/// Which part of a [`Permit`] a [`Gate`] watches.
enum Part {
	/// The peer's grant and the scope both.
	Both,
	/// The peer's grant alone: a token the peer presented stands in for the scope.
	Granted,
	/// The peer's grant, and the scope once the announce that stood in for it closes.
	Announced(kio::Consumer<()>),
}

impl Gate {
	pub(crate) fn new(handle: Link, path: crate::Path, direction: Direction) -> Self {
		let within_limit = handle.within_limit(direction, path.as_str());
		Self {
			handle,
			path,
			direction,
			epoch: 0,
			part: Part::Both,
			within_limit,
			admitted: false,
			waiting: None,
		}
	}

	/// Hold the request to the peer's grant alone, once its own token authorized it.
	pub(crate) fn named(mut self) -> Self {
		self.part = Part::Granted;
		self
	}

	/// A gate for a request under a namespace the peer announced, while `live` stays open:
	/// the announce stands in for the scope, as its own token may have authorized it.
	pub(crate) fn announced(handle: Link, path: crate::Path, direction: Direction, live: kio::Consumer<()>) -> Self {
		Self {
			part: Part::Announced(live),
			..Self::new(handle, path, direction)
		}
	}

	/// Whether the session allows the path right now.
	pub(crate) fn allows(&self) -> bool {
		let path = self.path.as_str();
		self.handle.within_grant(self.direction, path)
			&& match &self.part {
				Part::Both => self.within_limit,
				Part::Granted => true,
				Part::Announced(live) => self.within_limit || !live.is_closed(),
			}
	}

	/// Whether the peer's request token must be verified separately from session scope.
	pub(crate) fn token_decides(&self) -> bool {
		self.handle.token_decides(self.direction)
	}

	/// Verify an incoming request's token against the local acceptor.
	pub(crate) fn verify(&self, token: crate::setup::Token, hold: Option<Hold>) -> RequestVerdict {
		self.handle.verify_request(token, hold)
	}

	/// What the session allows in this gate's direction, once it may have changed since `epoch`.
	pub(crate) fn poll_permit(&self, epoch: &mut u64, waiter: &kio::Waiter) -> Poll<Permit> {
		self.handle.poll_permit(self.direction, epoch, waiter)
	}

	/// Preserve setup ordering before the first discovery snapshot.
	pub(crate) fn poll_setup_answered(&self, waiter: &kio::Waiter) -> Poll<()> {
		self.handle.poll_setup_answered(waiter)
	}

	/// Settle tokens that might admit this request before its first wire operation.
	pub(crate) fn poll_ready(&mut self, waiter: &kio::Waiter) -> Poll<()> {
		if self.admitted {
			return Poll::Ready(());
		}
		// A withdrawn route ends the wait too: the request is then judged against the union
		// as it stands, so routing can recover instead of parking on a token.
		let withdrawn = matches!(&self.part, Part::Announced(live) if live.poll_closed(waiter).is_ready());
		if !withdrawn
			&& self
				.handle
				.poll_presented(self.direction, self.path.as_str(), waiter)
				.is_pending()
		{
			self.waiting.get_or_insert_with(|| Waiting {
				path: self.path.clone(),
				started: Instant::now(),
			});
			return Poll::Pending;
		}
		drop(self.waiting.take());
		self.admitted = true;
		Poll::Ready(())
	}

	/// Resolve one operation under this gate without dropping the handler's refusal path.
	#[cfg(feature = "async")]
	pub(crate) async fn guard<T>(&mut self, operation: impl std::future::Future<Output = Result<T>>) -> Result<T> {
		let mut operation = std::pin::pin!(operation);
		kio::wait(|waiter| {
			std::task::ready!(self.poll_ready(waiter));
			if self.poll_denied(waiter).is_ready() {
				return Poll::Ready(Err(Error::Unauthorized));
			}
			waiter.poll_future(operation.as_mut())
		})
		.await
	}

	/// Resolve once the session no longer allows the path. A union that is still
	/// `None` (no answer yet, or a version without AUTH) and a session never limited
	/// allow everything.
	pub(crate) fn poll_denied(&mut self, waiter: &kio::Waiter) -> Poll<()> {
		while let Poll::Ready(permit) = self.handle.poll_permit(self.direction, &mut self.epoch, waiter) {
			let path = self.path.as_str();
			self.within_limit = permit.within_limit(path);
			let allowed = match self.part {
				Part::Both => permit.matches(path),
				Part::Granted | Part::Announced(_) => permit.granted(path),
			};
			if !allowed {
				return Poll::Ready(());
			}
		}
		match &self.part {
			Part::Announced(live) if !self.within_limit => live.poll_closed(waiter),
			_ => Poll::Pending,
		}
	}
}

/// The default acceptor's answer to the peer's connection credential: the grant
/// the session's origin handles allow, within the limit as it changes.
pub(crate) struct DefaultGrant {
	handle: Link,
	base: Grant,
	epoch: u64,
	limit: Option<Grant>,
	/// The grant last handed out, so an unrelated wakeup sends nothing.
	sent: Option<Grant>,
}

impl DefaultGrant {
	pub(crate) fn new(handle: Link, base: Grant) -> Self {
		let limit = handle.state().read().limit.clone();
		Self {
			handle,
			base,
			epoch: 0,
			limit,
			sent: None,
		}
	}

	/// The grant to send next: the first, then each one a new limit changes.
	pub(crate) fn poll(&mut self, waiter: &kio::Waiter) -> Poll<Grant> {
		while let Poll::Ready(limit) = self.handle.poll_limit(&mut self.epoch, waiter) {
			self.limit = limit;
		}
		let grant = match &self.limit {
			Some(limit) => self.base.intersect(limit),
			None => self.base.clone(),
		};
		if self.sent.as_ref() == Some(&grant) {
			return Poll::Pending;
		}
		self.sent = Some(grant.clone());
		Poll::Ready(grant)
	}
}

/// The close reason naming a broadcast published outside the grant. [`Error`] carries
/// no payload, so the path travels here, in the session's own terms.
pub(crate) fn unauthorized_reason(path: &crate::Path) -> String {
	format!("unauthorized: {path}")
}

/// Finds a broadcast this side publishes that its grant never covered, so the session
/// can fail loudly instead of waiting for a subscription that never comes.
///
/// Waits until the tokens the session presented at setup are answered, then checks
/// each broadcast when it is first announced. A grant that later shrinks withdraws what
/// it no longer covers without aborting: the union is processed before new
/// announcements, so a revocation is never mistaken for a new unauthorized
/// publication. Only the dialing side enforces: a server's publish origin is everything
/// the peer may read, not what it intends to push.
#[derive(Default)]
pub(crate) struct Enforce {
	epoch: u64,
	permit: Option<Patterns>,
	/// Every broadcast admitted so far, still announced.
	live: std::collections::HashSet<crate::Path>,
	setup: bool,
	/// Broadcasts outside the union, still announced, waiting on tokens that might cover
	/// them, in announcement order.
	pending: Vec<Gate>,
}

impl Enforce {
	/// Resolve with the first broadcast announced outside the union, or `None` once the
	/// origin ends.
	pub(crate) fn poll(
		&mut self,
		handle: &Link,
		announced: &mut crate::announce::Consumer,
		waiter: &kio::Waiter,
	) -> Poll<Option<crate::Path>> {
		if !self.setup {
			ready_or!(handle.poll_setup_answered(waiter));
			self.setup = true;
		}
		while let Poll::Ready(union) = handle.poll_union(&mut self.epoch, waiter) {
			self.permit = union.map(|grant| grant.publish);
		}
		// No grant yet (the peer never answered with one): nothing to check against.
		if self.permit.is_none() {
			return Poll::Pending;
		}

		// Withdrawals queued behind a wait apply before it is judged, so a broadcast that is
		// no longer published never closes the session.
		while let Poll::Ready(event) = announced.poll_next(waiter) {
			let Some(event) = event else {
				return Poll::Ready(None);
			};
			match event {
				crate::announce::Event::Start(update) | crate::announce::Event::Update(update)
					if !self.live.contains(&update.prefix) =>
				{
					if self.pending.iter().any(|gate| gate.path == update.prefix) {
						continue;
					}
					let mut gate = Gate::new(handle.clone(), update.prefix, Direction::Publish);
					if gate.poll_ready(waiter).is_pending() {
						self.pending.push(gate);
						continue;
					}
					if !handle.within_grant(Direction::Publish, gate.path.as_str()) {
						return Poll::Ready(Some(gate.path));
					}
					self.live.insert(gate.path);
				}
				crate::announce::Event::End(update) => {
					self.live.remove(&update.prefix);
					self.pending.retain(|gate| gate.path != update.prefix);
				}
				_ => {}
			}
		}

		while let Some(index) = self
			.pending
			.iter_mut()
			.position(|gate| gate.poll_ready(waiter).is_ready())
		{
			let gate = self.pending.remove(index);
			if !handle.within_grant(Direction::Publish, gate.path.as_str()) {
				return Poll::Ready(Some(gate.path));
			}
			self.live.insert(gate.path);
		}
		Poll::Pending
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[moq_clock::test]
	async fn a_request_waits_for_an_unanswered_token() {
		let handle = Link::new(true);
		let setup = handle.present(Bytes::new(), true).unwrap();
		handle.granted(setup.presented.id, Grant::default());
		let token = handle.present(Bytes::from_static(b"new"), false).unwrap();
		let mut gate = Gate::new(handle.clone(), crate::Path::from_static("room"), Direction::Subscribe);
		let waiter = kio::Waiter::noop();
		assert!(
			gate.poll_ready(&waiter).is_pending(),
			"request refused before token answered"
		);
		handle.granted(token.presented.id, Grant::all());
		assert!(gate.poll_ready(&waiter).is_ready());
		assert!(gate.poll_denied(&waiter).is_pending());
	}

	/// A token waiting out a transient refusal is unanswered, so a publication it might
	/// cover waits for the retry instead of being judged unauthorized.
	#[test]
	fn a_retrying_token_holds_a_publication() {
		let handle = Link::new(true);
		let _token = handle.present(Bytes::from_static(b"jwt"), false).unwrap();
		let id = opened(&handle);
		handle.refused(id);
		let busy = Error::Session(SessionError::TooManyRequests);
		assert_eq!(handle.retry(id, &busy), Some(RETRY_INITIAL));

		let mut gate = Gate::new(handle.clone(), crate::Path::from_static("room"), Direction::Publish);
		let waiter = kio::Waiter::noop();
		assert!(
			gate.poll_ready(&waiter).is_pending(),
			"judged before the retry answered"
		);
		handle.reopen(id).unwrap();
		handle.granted(id, Grant::all());
		assert!(gate.poll_ready(&waiter).is_ready());
		assert!(handle.within_grant(Direction::Publish, "room"));
	}

	/// A route withdrawn while its request waits on a token ends the wait, and the request
	/// is refused against the union as it stands.
	#[test]
	fn a_withdrawn_route_ends_the_wait() {
		let handle = Link::new(true);
		let setup = handle.present(Bytes::new(), true).unwrap();
		handle.granted(setup.presented.id, Grant::default());
		let _unanswered = handle.present(Bytes::from_static(b"new"), false).unwrap();
		let route = kio::Producer::<()>::default();
		let mut gate = Gate::announced(
			handle,
			crate::Path::from_static("room"),
			Direction::Subscribe,
			route.consume(),
		);
		let waiter = kio::Waiter::noop();
		assert!(gate.poll_ready(&waiter).is_pending(), "waits while the route stands");
		drop(route);
		assert!(gate.poll_ready(&waiter).is_ready(), "a withdrawn route kept the wait");
		assert!(gate.poll_denied(&waiter).is_ready());
	}

	#[test]
	fn an_empty_token_never_holds_a_request() {
		let handle = Link::new(true);
		let _setup = handle.present(Bytes::new(), true).unwrap();
		let mut gate = Gate::new(handle, crate::Path::from_static("room"), Direction::Subscribe);
		assert!(gate.poll_ready(&kio::Waiter::noop()).is_ready());
	}

	#[test]
	fn a_covered_request_never_waits_for_an_unanswered_token() {
		let handle = Link::new(true);
		let first = handle.present(Bytes::from_static(b"first"), false).unwrap();
		handle.granted(first.presented.id, Grant::all());
		let _unanswered = handle.present(Bytes::from_static(b"renewal"), false).unwrap();
		let mut gate = Gate::new(handle, crate::Path::from_static("room"), Direction::Subscribe);
		assert!(gate.poll_ready(&kio::Waiter::noop()).is_ready());
	}

	#[test]
	fn an_auth_ok_is_queued_after_its_scope_widens() {
		let handle = Link::new(true);
		handle.handle().authorize(&Grant::default());
		let serving = Serving::new(handle.clone());
		let request = Request::new(Bytes::from_static(b"token"), serving.issue.clone());
		let _issued = request.accept(Grant::all());
		let issue = serving.issue.read();
		assert!(matches!(issue.outbox.front(), Some(Reply::Grant(_))));
		assert!(handle.within_limit(Direction::Subscribe, "room"));
	}

	/// A queued request token keeps its request's slot, so the peer's request credit bounds
	/// how many wait; answering it gives the slot back.
	#[test]
	fn a_queued_request_token_holds_its_slot() {
		let handle = Link::new(true);
		let mut requests = handle.handle().requests();
		let slot: Hold = std::sync::Arc::new(());
		let token = crate::setup::Token {
			kind: crate::setup::Token::OUT_OF_BAND,
			value: b"jwt".to_vec(),
		};
		let _verdict = handle.verify_request(token, Some(slot.clone()));
		assert_eq!(
			std::sync::Arc::strong_count(&slot),
			2,
			"queued, the token holds the slot"
		);
		let Poll::Ready(Some(request)) = requests.poll_next(&kio::Waiter::noop()) else {
			panic!("request token missing")
		};
		assert_eq!(
			std::sync::Arc::strong_count(&slot),
			2,
			"taken, the token still holds the slot"
		);
		let _issued = request.accept(Grant::all());
		assert_eq!(std::sync::Arc::strong_count(&slot), 1, "answered, the slot is released");
	}

	/// Withdrawn tokens leave the queue as it grows, so a peer that keeps presenting and
	/// withdrawing cannot grow it past what its credit holds open.
	#[test]
	fn withdrawn_tokens_leave_the_queue() {
		let handle = Link::new(true);
		let mut requests = handle.handle().requests();
		let queue = handle.acceptor().unwrap();
		queue.push(Request::new(Bytes::from_static(b"live"), Default::default()));
		let withdrawn = kio::Shared::<Issue>::default();
		withdrawn.lock().peer = Some(Error::Cancel);
		for _ in 0..1000 {
			queue.push(Request::new(Bytes::from_static(b"withdrawn"), withdrawn.clone()));
			assert!(queue.state.read().requests.len() <= 3, "withdrawn tokens piled up");
		}
		let Poll::Ready(Some(request)) = requests.poll_next(&kio::Waiter::noop()) else {
			panic!("live request missing")
		};
		assert_eq!(request.token().as_ref(), b"live");
		assert!(requests.poll_next(&kio::Waiter::noop()).is_pending());
	}

	fn patterns(prefixes: &[&str]) -> Patterns {
		prefixes
			.iter()
			.map(|prefix| crate::Pattern::subtree(prefix).unwrap())
			.collect()
	}

	fn grant(publish: &[&str], subscribe: &[&str]) -> Grant {
		Grant {
			publish: patterns(publish),
			subscribe: patterns(subscribe),
			expires: None,
		}
	}

	/// A limit replaces the last one, narrower or wider, and applies to a version
	/// without AUTH too.
	#[test]
	fn a_limit_replaces_the_last() {
		let handle = Link::new(false);
		assert!(handle.allows(Direction::Publish, "room/alice/audio"));

		handle.handle().authorize(&grant(&["room/bob"], &["room/alice"]));
		// What we send is what the peer may subscribe to, and the other way around.
		assert!(handle.allows(Direction::Publish, "room/alice/audio"));
		assert!(!handle.allows(Direction::Publish, "room/bob/cam"));
		assert!(handle.allows(Direction::Subscribe, "room/bob/cam"));
		assert!(!handle.allows(Direction::Subscribe, "room/alice/audio"));

		handle.handle().authorize(&grant(&[""], &["room/alice/video"]));
		assert!(!handle.allows(Direction::Publish, "room/alice/audio"));
		assert!(handle.allows(Direction::Publish, "room/alice/video"));
		assert!(handle.allows(Direction::Subscribe, "other"));

		handle.handle().authorize(&Grant::all());
		assert!(handle.allows(Direction::Publish, "room/alice/audio"));
		assert!(handle.allows(Direction::Subscribe, "other"));
	}

	/// Our own grant does not decide what the peer may offer us, only the limit does.
	#[test]
	fn the_limit_alone_decides_what_the_peer_offers() {
		let handle = Link::new(true);
		let token = handle.present(Bytes::new(), true).unwrap();
		handle.granted(0, grant(&[], &[]));
		assert!(!handle.allows(Direction::Subscribe, "room/bob/cam"));
		assert!(handle.within_limit(Direction::Subscribe, "room/bob/cam"));

		handle.handle().authorize(&grant(&["room/alice"], &[]));
		assert!(!handle.within_limit(Direction::Subscribe, "room/bob/cam"));
		assert!(handle.within_limit(Direction::Subscribe, "room/alice/cam"));
		drop(token);
	}

	/// The default acceptor answers with its base grant, then with each grant a new
	/// limit changes, narrower or wider, and stays quiet on a change that leaves it alone.
	#[test]
	fn the_default_grant_follows_the_limit() {
		let handle = Link::new(true);
		let mut default = DefaultGrant::new(handle.clone(), grant(&["room"], &["room"]));
		let waiter = kio::Waiter::noop();

		assert_eq!(default.poll(&waiter), Poll::Ready(grant(&["room"], &["room"])));
		assert!(default.poll(&waiter).is_pending());

		handle.handle().authorize(&grant(&["room"], &["room/alice"]));
		assert_eq!(default.poll(&waiter), Poll::Ready(grant(&["room"], &["room/alice"])));

		// Wider again, but never past what the origin handles allow.
		handle.handle().authorize(&Grant::all());
		assert_eq!(default.poll(&waiter), Poll::Ready(grant(&["room"], &["room"])));

		// A limit that leaves the grant as it is: nothing to tell the peer.
		handle.handle().authorize(&grant(&[""], &[""]));
		assert!(default.poll(&waiter).is_pending());
	}

	/// On moq-transport without AUTH an auth needs no answer: it is kept for the requests
	/// that name it, with a grant that stays unknown. moq-lite without AUTH has nothing that
	/// could carry it.
	#[test]
	fn a_request_carries_an_auth_without_the_auth_stream() {
		let ietf = Link::ietf(false);
		let auth = ietf.present(Bytes::from_static(b"jwt"), false).unwrap();
		assert!(matches!(
			auth.poll_answered(&kio::Waiter::noop()),
			Poll::Ready(Ok(Answer::Requests))
		));
		assert_eq!(auth.grant().peek(), None);
		assert_eq!(ietf.token(&auth).as_deref(), Some(b"jwt".as_slice()));
		assert!(
			ietf.read(|link, _| link.opening.is_empty()),
			"nothing to open a stream for"
		);

		let lite = Link::new(false);
		let auth = lite.present(Bytes::from_static(b"jwt"), false).unwrap();
		assert!(matches!(
			auth.poll_answered(&kio::Waiter::noop()),
			Poll::Ready(Err(Error::Unsupported))
		));
	}

	/// A peer that turns out not to speak AUTH leaves a moq-transport auth for its
	/// requests rather than failing it.
	#[test]
	fn an_unsupported_peer_keeps_a_request_auth() {
		let handle = Link::ietf(true);
		let auth = handle.present(Bytes::from_static(b"jwt"), false).unwrap();
		assert!(auth.poll_answered(&kio::Waiter::noop()).is_pending());
		handle.unsupported();
		assert!(matches!(
			auth.poll_answered(&kio::Waiter::noop()),
			Poll::Ready(Ok(Answer::Requests))
		));
		assert_eq!(handle.token(&auth).as_deref(), Some(b"jwt".as_slice()));
	}

	/// Only the handle that issued an auth carries it: a route naming another session's
	/// auth goes out under this session's scope.
	#[test]
	fn another_handle_does_not_carry_an_auth() {
		let issuer = Link::ietf(false);
		let auth = issuer.present(Bytes::from_static(b"jwt"), false).unwrap();
		assert_eq!(Link::ietf(false).token(&auth), None);
	}

	/// Every clone names one credential, which stays presented until the last drops.
	#[test]
	fn the_last_clone_withdraws_the_auth() {
		let handle = Link::new(true);
		let auth = handle.present(Bytes::from_static(b"jwt"), false).unwrap();
		let Poll::Ready(Some((id, _))) = handle.poll_opening(&kio::Waiter::noop()) else {
			panic!("nothing to open");
		};
		let clone = auth.clone();
		assert_eq!(clone, auth, "a clone is the same auth");
		assert_ne!(
			handle.present(Bytes::from_static(b"jwt"), false).unwrap(),
			auth,
			"the same bytes presented again are another auth"
		);

		drop(auth);
		assert!(
			handle.poll_withdrawn(id, &kio::Waiter::noop()).is_pending(),
			"withdrawn while a clone lives"
		);
		drop(clone);
		assert!(handle.poll_withdrawn(id, &kio::Waiter::noop()).is_ready());
	}

	fn opened(link: &Link) -> u64 {
		match link.poll_opening(&kio::Waiter::noop()) {
			Poll::Ready(Some((id, _))) => id,
			_ => panic!("nothing to open"),
		}
	}

	/// Only the peer's verdict ends a handle's token for good: a session ending leaves it for
	/// the next session, which presents it again, and one active session at a time.
	#[test]
	fn only_a_verdict_ends_a_shared_token() {
		let handle = Handle::new();
		let first = Link::attach(&handle, true, false).unwrap();
		assert!(matches!(Link::attach(&handle, true, false), Err(Error::Duplicate)));
		let auth = handle.present(Bytes::from_static(b"jwt")).unwrap();
		let id = opened(&first);
		first.granted(id, Grant::all());
		assert!(matches!(
			auth.poll_answered(&kio::Waiter::noop()),
			Poll::Ready(Ok(Answer::Granted))
		));
		assert_eq!(auth.grant().peek(), Some(Grant::all()));

		first.close(Error::Cancel);
		assert_eq!(auth.grant().peek(), None, "no session is live");
		assert!(Selector::Token(id).ended(&handle.state.read()).is_none());

		let second = Link::attach(&handle, true, false).unwrap();
		assert_eq!(opened(&second), id, "the next session presents it again");
		second.refused(id);
		let refused = Error::Session(SessionError::Unauthorized);
		assert_eq!(second.retry(id, &refused), None);
		second.ended(id, refused);
		assert!(matches!(
			Selector::Token(id).ended(&handle.state.read()),
			Some(Error::Session(SessionError::Unauthorized))
		));

		second.close(Error::Cancel);
		let third = Link::attach(&handle, true, false).unwrap();
		assert!(
			third.poll_opening(&kio::Waiter::noop()).is_pending(),
			"a refused token stays ended"
		);
	}

	/// The answer records how the peer accepted a token, so a grant whose session drops before
	/// the caller reads it is still told apart from a token only requests carry.
	#[test]
	fn the_answer_outlives_its_session() {
		let handle = Handle::new();
		let session = Link::attach(&handle, true, false).unwrap();
		let auth = handle.present(Bytes::from_static(b"jwt")).unwrap();
		session.granted(opened(&session), Grant::all());
		session.close(Error::Cancel);
		assert_eq!(auth.grant().peek(), None);
		assert!(matches!(
			auth.poll_answered(&kio::Waiter::noop()),
			Poll::Ready(Ok(Answer::Granted))
		));

		let handle = Handle::new();
		let _session = Link::attach(&handle, false, true).unwrap();
		let auth = handle.present(Bytes::from_static(b"jwt")).unwrap();
		assert!(matches!(
			auth.poll_answered(&kio::Waiter::noop()),
			Poll::Ready(Ok(Answer::Requests))
		));
	}

	/// A session's own handle has no successor: a token added after its GOAWAY fails at once,
	/// and one whose stream ends without a verdict ends for good.
	#[test]
	fn a_sessions_own_handle_ends_its_tokens() {
		let link = Link::new(true);
		let auth = link.present(Bytes::from_static(b"jwt"), false).unwrap();
		let id = opened(&link);
		let lost = Error::Stream(crate::StreamError::Internal);
		assert_eq!(link.retry(id, &lost), None, "the application hears of a lost token");
		link.ended(id, lost);
		assert!(matches!(
			auth.poll_answered(&kio::Waiter::noop()),
			Poll::Ready(Err(Error::Stream(_)))
		));

		let (goaway, _protocol) = crate::goaway::Handle::new(false);
		goaway.producer().send(crate::goaway::Goaway::new()).unwrap();
		link.drain_with(std::sync::Arc::new(goaway));
		assert!(matches!(
			link.present(Bytes::from_static(b"late"), false),
			Err(Error::GoingAway)
		));
	}

	/// A token added while a session without AUTH drains rides only its successor, so the
	/// draining session cannot end it as unsupported.
	#[test]
	fn a_draining_session_takes_no_new_token() {
		let handle = Handle::new();
		let draining = Link::attach(&handle, false, false).unwrap();
		let (goaway, _protocol) = crate::goaway::Handle::new(false);
		goaway.producer().send(crate::goaway::Goaway::new()).unwrap();
		draining.drain_with(std::sync::Arc::new(goaway));
		let successor = Link::attach(&handle, true, false).unwrap();

		let auth = handle.present(Bytes::from_static(b"jwt")).unwrap();
		assert!(Selector::Token(auth.id()).ended(&handle.state.read()).is_none());
		let id = opened(&successor);
		successor.granted(id, Grant::all());
		assert!(matches!(
			auth.poll_answered(&kio::Waiter::noop()),
			Poll::Ready(Ok(Answer::Granted))
		));
	}

	/// A draining session's late refusal ends the token there alone while its successor
	/// presents it, so a stale verdict never withdraws a live grant.
	#[test]
	fn a_draining_refusal_leaves_the_successors_grant() {
		let handle = Handle::new();
		let draining = Link::attach(&handle, true, false).unwrap();
		let auth = handle.present(Bytes::from_static(b"jwt")).unwrap();
		let id = opened(&draining);
		let (goaway, _protocol) = crate::goaway::Handle::new(false);
		goaway.producer().send(crate::goaway::Goaway::new()).unwrap();
		draining.drain_with(std::sync::Arc::new(goaway));

		let successor = Link::attach(&handle, true, false).unwrap();
		assert_eq!(opened(&successor), id);
		successor.granted(id, Grant::all());
		draining.refused(id);
		draining.ended(id, Error::Session(SessionError::Unauthorized));
		assert!(Selector::Token(id).ended(&handle.state.read()).is_none());
		assert_eq!(auth.grant().peek(), Some(Grant::all()));
	}

	/// A draining session's late refusal leaves a token its successor is still deciding,
	/// including one the successor granted and then transiently refused.
	#[test]
	fn a_draining_refusal_defers_to_the_successors_retry() {
		let handle = Handle::new();
		let draining = Link::attach(&handle, true, false).unwrap();
		let auth = handle.present(Bytes::from_static(b"jwt")).unwrap();
		let id = opened(&draining);
		let (goaway, _protocol) = crate::goaway::Handle::new(false);
		goaway.producer().send(crate::goaway::Goaway::new()).unwrap();
		draining.drain_with(std::sync::Arc::new(goaway));

		let successor = Link::attach(&handle, true, false).unwrap();
		assert_eq!(opened(&successor), id);
		successor.granted(id, Grant::all());
		successor.refused(id);
		let busy = Error::Session(SessionError::TooManyRequests);
		assert_eq!(successor.retry(id, &busy), Some(RETRY_INITIAL));

		draining.refused(id);
		draining.ended(id, Error::Session(SessionError::Unauthorized));
		assert!(Selector::Token(id).ended(&handle.state.read()).is_none());

		assert_eq!(successor.reopen(id).unwrap().as_ref(), b"jwt");
		successor.granted(id, Grant::all());
		assert_eq!(auth.grant().peek(), Some(Grant::all()));
	}

	/// A retry that wakes after this side sent a GOAWAY opens nothing: a successor presents
	/// it, and on a session's own handle the token ends as going away, not as a clean close.
	#[test]
	fn a_draining_session_presents_no_retry() {
		let link = Link::new(true);
		let (goaway, _protocol) = crate::goaway::Handle::new(false);
		let goaway = std::sync::Arc::new(goaway);
		link.drain_with(goaway.clone());
		let auth = link.present(Bytes::from_static(b"jwt"), false).unwrap();
		let id = opened(&link);
		link.refused(id);
		let busy = Error::Session(SessionError::TooManyRequests);
		assert_eq!(link.retry(id, &busy), Some(RETRY_INITIAL));

		goaway.producer().send(crate::goaway::Goaway::new()).unwrap();
		assert!(!link.active());
		let err = link.reopen(id).unwrap_err();
		assert!(matches!(err, Error::GoingAway), "{err:?}");
		link.ended(id, err);
		assert!(matches!(
			Selector::Token(auth.id()).ended(&link.state().read()),
			Some(Error::GoingAway)
		));
	}

	/// A shared handle's token whose stream is lost without a verdict is presented again on the
	/// same session, doubling its backoff until a grant resets it. A request keeps carrying it
	/// meanwhile, and a request it might cover waits for the retry's answer.
	#[test]
	fn a_lost_stream_is_presented_again() {
		let handle = Handle::new();
		let link = Link::attach(&handle, true, true).unwrap();
		let auth = handle.present(Bytes::from_static(b"jwt")).unwrap();
		let id = opened(&link);
		let lost = Error::Stream(crate::StreamError::Internal);

		link.granted(id, Grant::all());
		assert_eq!(link.retry(id, &lost), Some(RETRY_INITIAL));
		assert_eq!(auth.grant().peek(), None, "the lost stream's grant is gone");
		assert_eq!(link.token(&auth).as_deref(), Some(&b"jwt"[..]));
		let mut gate = Gate::new(link.clone(), crate::Path::from_static("room"), Direction::Publish);
		assert!(gate.poll_ready(&kio::Waiter::noop()).is_pending());

		assert_eq!(link.reopen(id).unwrap().as_ref(), b"jwt");
		assert_eq!(link.retry(id, &lost), Some(RETRY_INITIAL * 2));
		link.reopen(id).unwrap();
		link.granted(id, Grant::all());
		assert!(gate.poll_ready(&kio::Waiter::noop()).is_ready());
		assert_eq!(link.retry(id, &lost), Some(RETRY_INITIAL), "a grant resets the backoff");
		assert!(Selector::Token(id).ended(&handle.state.read()).is_none());

		// A clean close after a grant is the peer's verdict, not a lost stream.
		link.reopen(id).unwrap();
		link.granted(id, Grant::all());
		assert_eq!(link.retry(id, &Error::Cancel), None);
		link.ended(id, Error::Cancel);
		assert!(matches!(
			Selector::Token(id).ended(&handle.state.read()),
			Some(Error::Cancel)
		));
	}

	/// A draining session presents nothing again, and a transient refusal there is no verdict:
	/// the token waits for the successor.
	#[test]
	fn a_draining_session_leaves_an_undecided_token() {
		let handle = Handle::new();
		let draining = Link::attach(&handle, true, false).unwrap();
		let _auth = handle.present(Bytes::from_static(b"jwt")).unwrap();
		let id = opened(&draining);
		let (goaway, _protocol) = crate::goaway::Handle::new(false);
		goaway.producer().send(crate::goaway::Goaway::new()).unwrap();
		draining.drain_with(std::sync::Arc::new(goaway));

		let busy = Error::Session(SessionError::TooManyRequests);
		draining.refused(id);
		assert_eq!(draining.retry(id, &busy), None);
		draining.ended(id, busy);
		assert!(Selector::Token(id).ended(&handle.state.read()).is_none());

		let successor = Link::attach(&handle, true, false).unwrap();
		assert_eq!(opened(&successor), id, "the successor presents it");
	}

	/// A refusal that may pass is presented again after a doubling backoff, and settles nothing.
	#[test]
	fn a_transient_refusal_backs_off() {
		let link = Link::new(true);
		let auth = link.present(Bytes::from_static(b"jwt"), false).unwrap();
		let id = opened(&link);
		let busy = Error::Session(SessionError::TooManyRequests);

		link.refused(id);
		assert_eq!(link.retry(id, &busy), Some(RETRY_INITIAL));
		assert!(auth.poll_answered(&kio::Waiter::noop()).is_pending());
		assert_eq!(link.reopen(id).unwrap().as_ref(), b"jwt");
		link.refused(id);
		assert_eq!(link.retry(id, &busy), Some(RETRY_INITIAL * 2));
		assert_eq!(link.retry(id, &busy), None, "only a refusal retries");

		link.reopen(id).unwrap();
		link.granted(id, Grant::all());
		assert!(matches!(
			auth.poll_answered(&kio::Waiter::noop()),
			Poll::Ready(Ok(Answer::Granted))
		));
	}
}

#[cfg(all(test, feature = "async"))]
mod request_token_tests {
	use super::*;

	fn token(value: &[u8], kind: u64) -> crate::setup::Token {
		crate::setup::Token {
			kind,
			value: value.to_vec(),
		}
	}

	fn subscribe_path() -> crate::Path {
		crate::Path::from_static("room/alice")
	}

	/// A request token reaches the app's acceptor like any other token, and the grant the
	/// app answers is what the verdict resolves to.
	#[moq_clock::test]
	async fn a_request_token_reaches_the_acceptor() {
		let handle = Link::new(true);
		let mut requests = handle.handle().requests();
		let verdict = handle.verify_request(token(b"jwt", 0), None);

		let request = requests.next().await.expect("a request");
		assert_eq!(request.token().as_ref(), b"jwt");
		let _issued = request.accept(Grant::all());

		let grant = verdict.grant().await.expect("granted");
		assert!(grant.publish.matches("room/alice"));
	}

	/// An AUTH token has no type, so a request token of any type but out-of-band cannot reach
	/// the acceptor as one: it is unsupported, never handed over as if it were a plain token.
	#[moq_clock::test]
	async fn a_typed_request_token_is_unsupported() {
		let handle = Link::new(true);
		let mut requests = handle.handle().requests();
		let verdict = handle.verify_request(token(b"cat", 1), None);
		let err = verdict.grant().await.expect_err("a typed token");
		assert!(matches!(err.error, Error::Unsupported), "{err:?}");
		assert!(
			requests.poll_next(&kio::Waiter::noop()).is_pending(),
			"nothing reached the acceptor"
		);
	}

	/// An empty request token is refused before the acceptor, which would read it as the
	/// connection's own credential.
	#[test]
	fn an_empty_request_token_is_unsupported() {
		let handle = Link::new(true);
		let mut requests = handle.handle().requests();
		let verdict = handle.verify_request(token(b"", crate::setup::Token::OUT_OF_BAND), None);
		assert!(matches!(
			verdict.poll_grant(&kio::Waiter::noop()),
			Poll::Ready(Err(Refusal {
				error: Error::Unsupported,
				..
			}))
		));
		assert!(
			requests.poll_next(&kio::Waiter::noop()).is_pending(),
			"nothing reached the acceptor"
		);
	}

	/// A request-borne token is verified even without the MoQ Auth extension: it rides the
	/// request message, not the AUTH stream, so `requests()` stays live on a session whose
	/// `supported` is false, even once the peer turns out not to speak AUTH. This is the shape
	/// a standard moq-transport peer presents when it does not negotiate the AUTH extension.
	#[moq_clock::test]
	async fn a_request_token_is_verified_without_the_auth_extension() {
		let handle = Link::new(false);
		let mut requests = handle.handle().requests();
		handle.unsupported();
		let verdict = handle.verify_request(token(b"jwt", 0), None);
		let request = requests.next().await.expect("a request reaches the acceptor");
		let _issued = request.accept(Grant::all());
		assert!(
			verdict.grant().await.expect("granted").publish.matches("room/alice"),
			"admitted without the AUTH extension"
		);
	}

	/// With no `requests()` consumer, a request token cannot be verified in band, so the verdict
	/// is Unsupported and the caller refuses the request NOT_SUPPORTED.
	#[moq_clock::test]
	async fn no_consumer_refuses_a_request_token_as_unsupported() {
		let handle = Link::new(true);
		let verdict = handle.verify_request(token(b"jwt", 0), None);
		let err = verdict.grant().await.expect_err("no verifier");
		assert!(matches!(err.error, Error::Unsupported), "{err:?}");
	}

	/// A refused request token resolves to the refusal, which the caller sends the peer as
	/// UNAUTHORIZED.
	#[moq_clock::test]
	async fn a_refused_request_token_is_unauthorized() {
		let handle = Link::new(true);
		let mut requests = handle.handle().requests();
		let verdict = handle.verify_request(token(b"jwt", 0), None);
		requests.next().await.unwrap().reject(SessionError::Unauthorized, "no");
		let err = verdict.grant().await.expect_err("refused");
		assert!(
			matches!(err.error, Error::Session(SessionError::Unauthorized)),
			"{err:?}"
		);
	}

	/// A grant the acceptor revoked, dropped or replaced before the verdict was read is admitted
	/// as it stands now, never as first issued.
	#[moq_clock::test]
	async fn a_grant_ended_before_admission_is_refused() {
		let handle = Link::new(true);
		let mut requests = handle.handle().requests();

		let verdict = handle.verify_request(token(b"jwt", 0), None);
		let issued = requests.next().await.unwrap().accept(Grant::all());
		issued.revoke(SessionError::Unauthorized, "revoked");
		let err = verdict.grant().await.expect_err("revoked before admission");
		assert_eq!(err.reason, "revoked");

		let verdict = handle.verify_request(token(b"jwt", 0), None);
		drop(requests.next().await.unwrap().accept(Grant::all()));
		let err = verdict.grant().await.expect_err("dropped before admission");
		assert!(
			matches!(err.error, Error::Session(SessionError::Unauthorized)),
			"{err:?}"
		);

		let verdict = handle.verify_request(token(b"jwt", 0), None);
		let issued = requests.next().await.unwrap().accept(Grant::all());
		issued.update(Grant::default());
		let grant = verdict.grant().await.expect("still granted");
		assert!(
			!grant.publish.matches("room/alice"),
			"the replacement is what is admitted"
		);
	}

	/// The grant lives exactly as long as the request: dropping the verdict ends the
	/// acceptor's issued grant, so it never outlives the request nor joins the union.
	#[moq_clock::test]
	async fn dropping_the_verdict_ends_the_grant() {
		let handle = Link::new(true);
		let mut requests = handle.handle().requests();
		let verdict = handle.verify_request(token(b"jwt", 0), None);
		let issued = requests.next().await.unwrap().accept(Grant::all());
		verdict.grant().await.expect("granted");
		drop(verdict);
		let err = issued.closed().await;
		assert!(matches!(err, Error::Cancel), "{err:?}");
	}

	/// A request token never joins the session union: the union stays what the connection
	/// credential earned, not what a request token granted.
	#[moq_clock::test]
	async fn a_request_token_never_joins_the_union() {
		let handle = Link::new(true);
		let mut requests = handle.handle().requests();
		let verdict = handle.verify_request(token(b"jwt", 0), None);
		let _issued = requests.next().await.unwrap().accept(Grant::all());
		verdict.grant().await.expect("granted");
		// The union only reflects tokens presented on this side (none here), never a
		// request token the acceptor answered.
		assert_eq!(
			handle.handle().grant().peek(),
			None,
			"a request token must not widen the union"
		);
	}

	fn subscribe_only(prefix: &str) -> Grant {
		Grant {
			publish: Patterns::new(),
			subscribe: crate::Pattern::subtree(prefix).unwrap().into(),
			expires: None,
		}
	}

	/// The session's scope for unnamed requests is the connection credential's grant and every
	/// grant issued to an AUTH token, and nothing a request token earned: a token on one
	/// request never widens what tokenless requests may do.
	#[moq_clock::test]
	async fn the_scope_is_session_tokens_only() {
		let handle = Link::new(true);
		let mut requests = handle.handle().requests();
		handle.handle().authorize(&subscribe_only("room/alice"));
		assert!(!handle.allows(Direction::Publish, "room/bob"));

		// A token on a request admits that request, and nothing else.
		let verdict = handle.verify_request(token(b"request", 0), None);
		let _request = requests.next().await.unwrap().accept(subscribe_only("room/bob"));
		verdict.grant().await.expect("granted");
		assert!(
			!handle.allows(Direction::Publish, "room/bob"),
			"a request token widened the scope"
		);

		// An AUTH token joins the scope while its grant stands.
		let serving = Serving::new(handle.clone());
		requests
			.queue
			.push(Request::new(Bytes::from_static(b"auth"), serving.issue.clone()));
		let issued = requests.next().await.unwrap().accept(subscribe_only("room/carol"));
		assert!(handle.allows(Direction::Publish, "room/carol"));
		assert!(
			handle.allows(Direction::Publish, "room/alice"),
			"the connection grant still stands"
		);

		issued.update(subscribe_only("room/dave"));
		assert!(
			!handle.allows(Direction::Publish, "room/carol"),
			"the update replaced it"
		);
		assert!(handle.allows(Direction::Publish, "room/dave"));

		// Withdrawn by the peer, the grant leaves the scope, and a late update cannot bring it back.
		serving.end(Error::Cancel);
		assert!(!handle.allows(Direction::Publish, "room/dave"));
		issued.update(subscribe_only("room/dave"));
		assert!(
			!handle.allows(Direction::Publish, "room/dave"),
			"a withdrawn token rejoined the scope"
		);
	}

	/// Revoking or dropping an AUTH token's grant takes it out of the scope.
	#[moq_clock::test]
	async fn an_ended_auth_grant_leaves_the_scope() {
		let handle = Link::new(true);
		let mut requests = handle.handle().requests();
		handle.handle().authorize(&Grant::default());
		for revoke in [true, false] {
			let serving = Serving::new(handle.clone());
			requests
				.queue
				.push(Request::new(Bytes::from_static(b"auth"), serving.issue.clone()));
			let issued = requests.next().await.unwrap().accept(subscribe_only("room"));
			assert!(handle.allows(Direction::Publish, "room/alice"));
			match revoke {
				true => issued.revoke(SessionError::Unauthorized, "revoked"),
				false => drop(issued),
			}
			assert!(!handle.allows(Direction::Publish, "room/alice"), "revoke={revoke}");
		}
	}

	/// A session never authorized is bound by its origin handles alone, whatever its AUTH
	/// tokens were granted: the scope only exists once the connection credential has a grant.
	#[moq_clock::test]
	async fn an_unauthorized_session_has_no_scope() {
		let handle = Link::new(true);
		let mut requests = handle.handle().requests();
		let serving = Serving::new(handle.clone());
		requests
			.queue
			.push(Request::new(Bytes::from_static(b"auth"), serving.issue.clone()));
		let _issued = requests.next().await.unwrap().accept(subscribe_only("room"));
		assert!(handle.allows(Direction::Publish, "other"));
	}

	use std::time::Duration;

	fn grant_in(runtime: &crate::time::Deadlines, secs: u64) -> Grant {
		Grant {
			publish: Patterns::new(),
			subscribe: crate::Pattern::all().into(),
			expires: runtime.now().checked_add(Duration::from_secs(secs)),
		}
	}

	/// The request grant carries the acceptor's expiry, which is the deadline it lapses at.
	#[moq_clock::test]
	async fn a_request_grant_has_the_acceptors_expiry() {
		let runtime = crate::time::sim();
		let handle = Link::new(true);
		let mut requests = handle.handle().requests();
		let verdict = handle.verify_request(token(b"jwt", 0), None);
		let grant = grant_in(&runtime, 60);
		let expires = grant.expires;
		let _issued = requests.next().await.unwrap().accept(grant);
		let answered = verdict.grant().await.unwrap();
		let request_grant = RequestGrant::new(
			&runtime,
			verdict,
			Ok(answered),
			subscribe_path(),
			RequestKind::Subscribe,
			None,
		)
		.unwrap();
		assert_eq!(request_grant.grant().expires, expires);
		assert!(expires.is_some());
	}

	/// A grant that has already expired never admits the request or renews it, even for the
	/// moment before its deadline would fire.
	#[moq_clock::test]
	async fn an_already_expired_grant_is_refused() {
		let runtime = crate::time::sim();
		let handle = Link::new(true);
		let mut requests = handle.handle().requests();
		let expired = || Grant {
			expires: Some(runtime.now()),
			..grant_in(&runtime, 0)
		};

		let verdict = handle.verify_request(token(b"old", 0), None);
		let _issued = requests.next().await.unwrap().accept(expired());
		let answered = verdict.grant().await;
		let refused = RequestGrant::new(
			&runtime,
			verdict,
			answered,
			subscribe_path(),
			RequestKind::Subscribe,
			None,
		);
		assert!(
			matches!(&refused, Err(refusal) if matches!(refusal.error, Error::Unauthorized)),
			"an expired grant admits nothing"
		);

		let verdict = handle.verify_request(token(b"jwt", 0), None);
		let _issued = requests.next().await.unwrap().accept(grant_in(&runtime, 60));
		let answered = verdict.grant().await;
		let request_grant = RequestGrant::new(
			&runtime,
			verdict,
			answered,
			subscribe_path(),
			RequestKind::Subscribe,
			None,
		)
		.unwrap();
		let renewal = handle.verify_request(token(b"old2", 0), None);
		let _issued = requests.next().await.unwrap().accept(expired());
		let renewed = renewal.grant().await;
		assert!(
			request_grant.renew(renewal, renewed).is_err(),
			"an expired grant renews nothing"
		);
	}

	/// An accepted REQUEST_UPDATE replaces the grant and its expiry.
	#[moq_clock::test]
	async fn a_renewal_token_replaces_the_grant_and_expiry() {
		let runtime = crate::time::sim();
		let handle = Link::new(true);
		let mut requests = handle.handle().requests();

		let verdict = handle.verify_request(token(b"jwt", 0), None);
		let first = grant_in(&runtime, 60);
		let first_expires = first.expires;
		let _issued = requests.next().await.unwrap().accept(first);
		let answered = verdict.grant().await.unwrap();
		let request_grant = RequestGrant::new(
			&runtime,
			verdict,
			Ok(answered),
			subscribe_path(),
			RequestKind::Subscribe,
			None,
		)
		.unwrap();

		// A REQUEST_UPDATE carrying a fresh token the acceptor accepts with a later expiry.
		let renewal = handle.verify_request(token(b"jwt2", 0), None);
		let second = grant_in(&runtime, 600);
		let second_expires = second.expires;
		let _issued2 = requests.next().await.unwrap().accept(second);
		let renewed = renewal.grant().await;
		assert!(
			request_grant.renew(renewal, renewed).is_ok(),
			"a covering renewal renews"
		);

		assert_eq!(request_grant.grant().expires, second_expires);
		assert_ne!(second_expires, first_expires);
	}

	/// With no accepted renewal, the deadline ends the request as Expired, and the
	/// session is not closed by it.
	#[moq_clock::test]
	async fn the_deadline_expires_the_request_without_renewal() {
		let runtime = crate::time::sim();
		let handle = Link::new(true);
		let mut requests = handle.handle().requests();
		let verdict = handle.verify_request(token(b"jwt", 0), None);
		// Held for the request's life, so only the deadline (not a drop) ends it.
		let _issued = requests.next().await.unwrap().accept(grant_in(&runtime, 60));
		let answered = verdict.grant().await.unwrap();
		let request_grant = RequestGrant::new(
			&runtime,
			verdict,
			Ok(answered),
			subscribe_path(),
			RequestKind::Subscribe,
			None,
		)
		.unwrap();

		let err = kio::wait(|waiter| request_grant.poll_ended(waiter)).await.error;
		assert!(matches!(err, Error::Expired), "{err:?}");
		assert_eq!(handle.handle().grant().peek(), None, "the session grant is untouched");
	}

	/// A renewal the acceptor granted before the old deadline lapsed keeps the request, even
	/// when the serve loop applies it only after the deadline.
	#[moq_clock::test]
	async fn a_granted_renewal_outlives_the_old_deadline() {
		let runtime = crate::time::sim();
		let handle = Link::new(true);
		let mut requests = handle.handle().requests();
		let verdict = handle.verify_request(token(b"jwt", 0), None);
		let _issued = requests.next().await.unwrap().accept(grant_in(&runtime, 60));
		let answered = verdict.grant().await.unwrap();
		let request_grant = RequestGrant::new(
			&runtime,
			verdict,
			Ok(answered),
			subscribe_path(),
			RequestKind::Subscribe,
			None,
		)
		.unwrap();

		let renewal = request_grant.verify_renewal(&handle, token(b"jwt2", 0));
		let second = grant_in(&runtime, 600);
		let second_expires = second.expires;
		let _issued2 = requests.next().await.unwrap().accept(second);
		moq_clock::sleep(Duration::from_secs(61)).await;

		let pending = kio::wait(|waiter| Poll::Ready(request_grant.poll_ended(waiter).is_pending())).await;
		assert!(pending, "the lapsed deadline ended a request whose renewal was granted");

		let renewed = renewal.grant().await;
		assert!(request_grant.renew(renewal, renewed).is_ok());
		assert_eq!(request_grant.grant().expires, second_expires);
		let pending = kio::wait(|waiter| Poll::Ready(request_grant.poll_ended(waiter).is_pending())).await;
		assert!(pending, "the renewal holds the request");
	}

	/// A refused renewal verdict is not applied: the request grant it already holds stands
	/// untouched and ends only when its own deadline lapses. The serve loops end the request on
	/// a refusal separately (a refused renewal ends only that request); this covers only that
	/// the grant object is not mutated.
	#[moq_clock::test]
	async fn a_refused_renewal_leaves_the_grant() {
		let runtime = crate::time::sim();
		let handle = Link::new(true);
		let mut requests = handle.handle().requests();
		let verdict = handle.verify_request(token(b"jwt", 0), None);
		let first = grant_in(&runtime, 60);
		let first_expires = first.expires;
		let _issued = requests.next().await.unwrap().accept(first);
		let answered = verdict.grant().await.unwrap();
		let request_grant = RequestGrant::new(
			&runtime,
			verdict,
			Ok(answered),
			subscribe_path(),
			RequestKind::Subscribe,
			None,
		)
		.unwrap();

		// The renewal token is refused, so it renews nothing and the old grant is untouched.
		let renewal = handle.verify_request(token(b"bad", 0), None);
		requests
			.next()
			.await
			.unwrap()
			.reject(SessionError::Unauthorized, "bad token");
		let refused = renewal.grant().await;
		assert!(
			matches!(&refused, Err(refusal) if matches!(refusal.error, Error::Session(SessionError::Unauthorized)) && refusal.reason == "bad token"),
			"the acceptor's code and reason reach the caller: {refused:?}"
		);
		assert!(
			request_grant.renew(renewal, refused).is_err(),
			"a refusal does not renew"
		);

		// The old grant still stands with its original expiry, and the request ends only
		// when that lapses.
		assert_eq!(request_grant.grant().expires, first_expires);
		let err = kio::wait(|waiter| request_grant.poll_ended(waiter)).await.error;
		assert!(matches!(err, Error::Expired), "{err:?}");
	}

	/// An acceptor-side update that no longer covers the request ends it, even when the
	/// replacement has no expiry to lapse at; one that still covers it is folded in.
	#[moq_clock::test]
	async fn an_update_that_stops_covering_ends_the_request() {
		let runtime = crate::time::sim();
		let handle = Link::new(true);
		let mut requests = handle.handle().requests();
		let verdict = handle.verify_request(token(b"jwt", 0), None);
		let issued = requests.next().await.unwrap().accept(Grant::all());
		let answered = verdict.grant().await.unwrap();
		let request_grant = RequestGrant::new(
			&runtime,
			verdict,
			Ok(answered),
			subscribe_path(),
			RequestKind::Subscribe,
			None,
		)
		.unwrap();

		// A covering update (everything, still unexpiring) leaves the request standing.
		issued.update(Grant::all());
		let pending = kio::wait(|waiter| Poll::Ready(request_grant.poll_ended(waiter).is_pending())).await;
		assert!(pending, "a covering update keeps the request");

		issued.update(Grant::default());
		let err = moq_clock::timeout(
			Duration::from_secs(1),
			kio::wait(|waiter| request_grant.poll_ended(waiter)),
		)
		.await
		.expect("an uncovering update must end the request")
		.error;
		assert!(matches!(err, Error::Unauthorized), "{err:?}");
	}

	/// The grant is the presenting peer's: a read is covered by `subscribe`, an announce by
	/// `publish`, never the other way around.
	#[test]
	fn request_coverage_is_from_the_presenters_side() {
		let read_only = Grant {
			publish: Patterns::new(),
			subscribe: crate::Pattern::all().into(),
			expires: None,
		};
		let write_only = Grant {
			publish: crate::Pattern::all().into(),
			subscribe: Patterns::new(),
			expires: None,
		};
		assert!(RequestKind::Subscribe.covers(&read_only, "room"));
		assert!(!RequestKind::Subscribe.covers(&write_only, "room"));
		assert!(RequestKind::PublishNamespace.covers(&write_only, "room"));
		assert!(!RequestKind::PublishNamespace.covers(&read_only, "room"));
	}

	/// An announce routes its whole namespace, so a grant naming only the namespace itself
	/// does not cover it, while one naming its subtree does.
	#[test]
	fn an_announce_needs_its_whole_subtree() {
		let exact = Grant {
			publish: crate::Pattern::literal("room").unwrap().into(),
			subscribe: Patterns::new(),
			expires: None,
		};
		let subtree = Grant {
			publish: crate::Pattern::subtree("room").unwrap().into(),
			subscribe: Patterns::new(),
			expires: None,
		};
		assert!(!RequestKind::PublishNamespace.covers(&exact, "room"));
		assert!(RequestKind::PublishNamespace.covers(&subtree, "room"));
		assert!(RequestKind::PublishNamespace.covers(&subtree, "room/cam"));
		assert!(!RequestKind::PublishNamespace.covers(&subtree, "other"));
		// A SUBSCRIBE names one broadcast, which the exact grant's twin covers.
		let read = Grant {
			publish: Patterns::new(),
			subscribe: crate::Pattern::literal("room").unwrap().into(),
			expires: None,
		};
		assert!(RequestKind::Subscribe.covers(&read, "room"));
	}

	/// An acceptor-side revoke ends the request before the deadline, and does not close the
	/// session.
	#[moq_clock::test]
	async fn an_acceptor_revoke_ends_the_request() {
		let runtime = crate::time::sim();
		let handle = Link::new(true);
		let mut requests = handle.handle().requests();
		let verdict = handle.verify_request(token(b"jwt", 0), None);
		// A grant that never expires, so only the revoke can end the request.
		let issued = requests.next().await.unwrap().accept(Grant::all());
		let answered = verdict.grant().await.unwrap();
		let request_grant = RequestGrant::new(
			&runtime,
			verdict,
			Ok(answered),
			subscribe_path(),
			RequestKind::Subscribe,
			None,
		)
		.unwrap();

		issued.revoke(SessionError::Unauthorized, "revoked");
		let refusal = kio::wait(|waiter| request_grant.poll_ended(waiter)).await;
		assert!(
			matches!(refusal.error, Error::Session(SessionError::Unauthorized)),
			"{refusal:?}"
		);
		assert_eq!(refusal.reason, "revoked", "the acceptor's reason is kept");
		assert_eq!(handle.handle().grant().peek(), None, "the session grant is untouched");
	}
}
