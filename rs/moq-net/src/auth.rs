//! In-band authorization: present tokens to the peer and learn what they grant.
//!
//! Each side of a session presents the credential its connection
//! already carried (the URL, a client certificate, or nothing) right after
//! setup, and learns the [`Grant`] it earned. `Session::auth`
//! returns the [`Handle`]: [`grant`](Handle::grant) is the union of every token
//! this side presented, and `add` presents another without
//! reconnecting. Dropping the returned [`Token`] withdraws it.
//!
//! The other direction, answering the peer's tokens, is automatic: the session
//! grants what its own origin handles allow. An application that verifies tokens
//! itself takes [`requests`](Handle::requests) before running the session's
//! driver, and then answers every token the peer presents. Either way,
//! [`authorize`](Handle::authorize) re-authorizes the peer on a live session, narrower
//! or wider, on every version, whether or not it speaks AUTH.
//!
//! Every token the peer presents is one kind of [`Request`], answered on the same queue,
//! wherever it rode: its own AUTH stream, or a moq-transport `AUTHORIZATION TOKEN` on a
//! SUBSCRIBE or PUBLISH_NAMESPACE. The session enforces each grant it issues. A request
//! that carried a token is held to that token's grant alone and ends with it; any other
//! request is held to the session's scope, which a token carried by a request never joins.
//!
//! moq-transport draft-17+ carries the same exchange when both sides negotiate the
//! MoQ Auth extension. Older versions, and peers that do not negotiate it, carry no
//! AUTH exchange: there the grant stays `None` and `add` fails with
//! [`Error::Unsupported`]. moq-lite carries a grant's patterns as they are;
//! moq-transport carries namespace prefixes, so it refuses a grant that is not a
//! union of subtrees rather than widen it.

use std::{
	collections::{BTreeMap, VecDeque},
	task::Poll,
};

use bytes::Bytes;

use crate::{Error, Patterns, Result, SessionError, time::Instant};

/// Classify malformed AUTH messages separately from transport and local failures.
#[cfg_attr(not(feature = "async"), allow(dead_code))]
pub(crate) fn protocol_error(err: Error) -> Error {
	match err {
		Error::Decode(_)
		| Error::BoundsExceeded(_)
		| Error::WrongSize
		| Error::TooManyParameters
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

	/// Fold `other` into this grant: the paths either allows, lapsing at the
	/// earlier expiry, which is when the union next shrinks.
	fn union(&mut self, other: &Self) {
		self.publish.extend(other.publish.iter().cloned());
		self.subscribe.extend(other.subscribe.iter().cloned());
		self.expires = earliest(self.expires, other.expires);
	}

	/// The paths both grants allow, lapsing at the earlier expiry.
	///
	/// Fails closed: an intersection too large to hold grants nothing, never more.
	#[cfg_attr(not(feature = "async"), allow(dead_code))]
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

/// The shared state behind every auth handle of one session.
#[derive(Default)]
pub(crate) struct State {
	/// Whether the negotiated version carries AUTH at all.
	supported: bool,
	/// Set once the session ends; every waiter resolves with it.
	closed: Option<Error>,
	#[cfg_attr(not(feature = "async"), allow(dead_code))]
	next_id: u64,
	/// Every token this side presented that has not ended.
	tokens: BTreeMap<u64, Slot>,
	/// Tokens waiting for the driver to open their stream.
	opening: VecDeque<u64>,
	/// The union of every open token's grant, `None` until the peer first replies.
	union: Option<Grant>,
	/// The peer replied to some token, so the union is known even when empty.
	replied: bool,
	/// What the peer's connection credential lets it do, in the peer's terms: `None` (only
	/// the origin handles bound it) until [`Handle::authorize`] sets it; the grant it was
	/// last called with.
	limit: Option<Grant>,
	/// The grants the application issued to the peer's AUTH tokens, by stream, in the
	/// peer's terms. A token on a single request never joins them.
	issued: BTreeMap<u64, Grant>,
	#[cfg_attr(not(feature = "async"), allow(dead_code))]
	next_issue: u64,
	/// The session's scope for the peer: the limit and every issued grant, `None` while the
	/// limit is.
	scope: Option<Grant>,
	/// Bumped whenever the union or the scope changes, so the session's per-stream
	/// gates can skip re-matching paths on every wakeup.
	epoch: u64,
	acceptor: Acceptor,
}

/// One presented token.
pub(crate) struct Slot {
	#[cfg_attr(not(feature = "async"), allow(dead_code))]
	token: Bytes,
	/// Presented by the session itself at setup, so enforcement waits on it.
	#[cfg_attr(not(feature = "async"), allow(dead_code))]
	setup: bool,
	/// The latest AUTH_OK, `None` before the first and after the token ends.
	grant: Option<Grant>,
	/// The first reply: `Ok` for an AUTH_OK, the refusal otherwise.
	#[cfg_attr(not(feature = "async"), allow(dead_code))]
	answered: Option<Result<()>>,
	/// Why the token ended, once it has.
	ended: Option<Error>,
	/// The [`Token`] handle dropped: the driver closes the stream.
	withdrawn: bool,
}

/// Who answers the peer's tokens. Decided once, when the driver first runs.
#[derive(Default)]
enum Acceptor {
	/// Nobody asked yet; the app may still take [`Handle::requests`].
	#[default]
	Undecided,
	/// The app answers every token.
	#[cfg_attr(not(feature = "async"), allow(dead_code))]
	App(Pending),
	/// The session answers from its own origin handles.
	#[cfg_attr(not(feature = "async"), allow(dead_code))]
	Default,
}

impl State {
	fn recompute(&mut self) {
		let mut granted = self.tokens.values().filter_map(|slot| slot.grant.as_ref()).peekable();
		if granted.peek().is_none() && !self.replied {
			return;
		}
		let mut union = Grant::default();
		for grant in granted {
			union.union(grant);
		}
		if self.union.as_ref() != Some(&union) {
			self.union = Some(union);
			self.epoch += 1;
		}
	}

	/// Re-derive the scope from the limit and the issued grants.
	fn rescope(&mut self) {
		let scope = self.limit.as_ref().map(|limit| {
			let mut scope = limit.clone();
			for grant in self.issued.values() {
				scope.union(grant);
			}
			scope
		});
		if self.scope != scope {
			self.scope = scope;
			self.epoch += 1;
		}
	}

	/// The error handed to anything waiting on a session that has ended.
	fn closed(&self) -> Option<Error> {
		self.closed.clone()
	}

	/// The union's and the scope's patterns for `direction`, each `None` while
	/// unrestricted.
	#[cfg_attr(not(feature = "async"), allow(dead_code))]
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
	#[cfg_attr(not(feature = "async"), allow(dead_code))]
	fn permit(&self, direction: Direction) -> Permit {
		let (granted, limit) = self.parts(direction);
		Permit {
			granted: granted.cloned(),
			limit: limit.cloned(),
		}
	}

	/// Whether `direction` allows `path` right now, without copying the patterns.
	#[cfg_attr(not(feature = "async"), allow(dead_code))]
	fn allows(&self, direction: Direction, path: &str) -> bool {
		let (granted, limit) = self.parts(direction);
		granted.is_none_or(|granted| granted.matches(path)) && limit.is_none_or(|limit| limit.matches(path))
	}
}

/// What one direction of a session allows: the grant the peer gave this side (the
/// union of its tokens) and the limit this side set on the peer, each `None`
/// while unrestricted.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
#[cfg_attr(not(feature = "async"), allow(dead_code))]
pub(crate) struct Permit {
	granted: Option<Patterns>,
	limit: Option<Patterns>,
}

impl Permit {
	/// Whether both allow `path`.
	#[cfg_attr(not(feature = "async"), allow(dead_code))]
	pub(crate) fn matches(&self, path: &str) -> bool {
		self.granted(path) && self.within_limit(path)
	}

	/// Whether the peer's grant alone allows `path`.
	pub(crate) fn granted(&self, path: &str) -> bool {
		self.granted.as_ref().is_none_or(|granted| granted.matches(path))
	}

	/// Whether the limit alone allows `path`: for what the peer offers us, which our
	/// own grant does not decide.
	#[cfg_attr(not(feature = "async"), allow(dead_code))]
	pub(crate) fn within_limit(&self, path: &str) -> bool {
		self.limit.as_ref().is_none_or(|limit| limit.matches(path))
	}

	/// Whether this allows everything `other` does, judged part by part, so a `false`
	/// may be spurious but a `true` never is.
	#[cfg_attr(not(feature = "async"), allow(dead_code))]
	pub(crate) fn covers(&self, other: &Self) -> bool {
		let covers = |this: &Option<Patterns>, other: &Option<Patterns>| match (this, other) {
			(None, _) => true,
			(Some(_), None) => false,
			(Some(this), Some(other)) => this.covers(other),
		};
		covers(&self.granted, &other.granted) && covers(&self.limit, &other.limit)
	}
}

/// The session's auth handle, returned by `Session::auth`.
///
/// Cheap to clone; every clone shares the session's tokens.
#[derive(Clone)]
pub struct Handle {
	state: kio::Shared<State>,
}

impl Handle {
	/// A handle for a session that speaks AUTH (`supported`) or never will.
	#[cfg_attr(not(feature = "async"), allow(dead_code))]
	pub(crate) fn new(supported: bool) -> Self {
		Self {
			state: kio::Shared::new(State {
				supported,
				..Default::default()
			}),
		}
	}

	/// Whether this side speaks AUTH: false on a version without it, on a side that does
	/// not offer it, and once the peer turns out not to.
	#[cfg_attr(not(feature = "async"), allow(dead_code))]
	pub(crate) fn supported(&self) -> bool {
		self.state.lock().supported
	}

	/// The union of every grant this side holds: `None` until the peer first
	/// answers a token (with a grant or a refusal), and forever on a version
	/// without AUTH.
	pub fn grant(&self) -> Watch {
		Watch::new(self.state.clone(), Selector::Union)
	}

	/// Present another token, resolving once the peer answers it.
	///
	/// The grant joins the union for as long as the returned [`Token`] lives.
	///
	/// # Errors
	///
	/// [`Error::Unsupported`] when the version or the peer carries no AUTH (or the
	/// peer takes no tokens in band), the peer's refusal as
	/// [`Error::Session`], or the session's close.
	#[cfg(feature = "async")]
	pub async fn add(&self, token: impl Into<Bytes>) -> Result<Token> {
		let token = self.present(token.into(), false)?;
		kio::wait(|waiter| token.poll_answered(waiter)).await?;
		Ok(token)
	}

	/// Queue a token for the driver to present.
	#[cfg_attr(not(feature = "async"), allow(dead_code))]
	pub(crate) fn present(&self, token: Bytes, setup: bool) -> Result<Token> {
		let mut state = self.state.lock();
		if !state.supported {
			return Err(Error::Unsupported);
		}
		if let Some(err) = state.closed() {
			return Err(err);
		}
		let id = state.next_id;
		state.next_id += 1;
		state.tokens.insert(
			id,
			Slot {
				token,
				setup,
				grant: None,
				answered: None,
				ended: None,
				withdrawn: false,
			},
		);
		state.opening.push_back(id);
		Ok(Token {
			state: self.state.clone(),
			id,
		})
	}

	/// Answer every token the peer presents, instead of the default acceptor: on its own
	/// AUTH stream, or riding one of its moq-transport SUBSCRIBE or PUBLISH_NAMESPACE
	/// requests as an `AUTHORIZATION TOKEN`.
	///
	/// Call it before running the session's driver: whoever answers is decided
	/// once, on the driver's first poll, so a later call cannot take over tokens
	/// already in flight. Without it the session grants the peer what its own
	/// origin handles allow for the connection's credential, and refuses any other
	/// token as unsupported, along with the request that carried it.
	///
	/// The peer's credit bounds how many tokens wait unanswered: each AUTH token holds its
	/// stream open, and each request token holds its request's slot, until it is answered or
	/// withdrawn. A withdrawn token leaves the queue.
	///
	/// # Errors
	///
	/// [`Error::Duplicate`] if the requests were already taken, or the driver
	/// already started answering them itself.
	pub fn requests(&self) -> Result<Requests> {
		let mut state = self.state.lock();
		if !matches!(state.acceptor, Acceptor::Undecided) {
			return Err(Error::Duplicate);
		}
		let queue = Pending::default();
		if state.closed.is_some() {
			queue.close();
		}
		state.acceptor = Acceptor::App(queue.clone());
		Ok(Requests { queue })
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
	#[cfg_attr(not(feature = "async"), allow(dead_code))]
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
	#[cfg_attr(not(feature = "async"), allow(dead_code))]
	pub(crate) fn acceptor(&self) -> Option<Pending> {
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

	/// The next token to open a stream for, or `Ready(None)` once the session ends.
	#[cfg_attr(not(feature = "async"), allow(dead_code))]
	pub(crate) fn poll_opening(&self, waiter: &kio::Waiter) -> Poll<Option<(u64, Bytes)>> {
		let mut state = ready_or!(self.state.poll(waiter, |state| {
			match state.opening.is_empty() && state.closed.is_none() {
				true => Poll::Pending,
				false => Poll::Ready(()),
			}
		}));
		if state.closed.is_some() {
			return Poll::Ready(None);
		}
		let id = state.opening.pop_front().expect("checked non-empty");
		let token = state.tokens.get(&id).map(|slot| slot.token.clone());
		match token {
			Some(token) => Poll::Ready(Some((id, token))),
			// Withdrawn before its stream opened: nothing to present.
			None => {
				drop(state);
				self.poll_opening(waiter)
			}
		}
	}

	/// Whether the token's handle dropped, so its stream should close.
	#[cfg_attr(not(feature = "async"), allow(dead_code))]
	pub(crate) fn poll_withdrawn(&self, id: u64, waiter: &kio::Waiter) -> Poll<()> {
		let _ = ready_or!(self.state.poll(waiter, |state| match state.tokens.get(&id) {
			Some(slot) if !slot.withdrawn => Poll::Pending,
			_ => Poll::Ready(()),
		}));
		Poll::Ready(())
	}

	/// Record an AUTH_OK for the token.
	#[cfg_attr(not(feature = "async"), allow(dead_code))]
	pub(crate) fn granted(&self, id: u64, grant: Grant) {
		// One parseable line per AUTH_OK, so the grant the peer actually sent is observable
		// without an API; the interop harness checks it against the token it minted.
		let list = |patterns: &Patterns| format!("{:?}", patterns.iter().map(|p| p.to_string()).collect::<Vec<_>>());
		tracing::debug!(
			publish = %list(&grant.publish),
			subscribe = %list(&grant.subscribe),
			"auth granted"
		);
		let mut state = self.state.lock();
		let Some(slot) = state.tokens.get_mut(&id) else {
			return;
		};
		slot.grant = Some(grant);
		slot.answered.get_or_insert(Ok(()));
		state.replied = true;
		state.recompute();
	}

	/// Record an AUTH_ERROR for the token: a reply that grants nothing, so a refused
	/// setup token leaves an empty union rather than an unknown (unrestricted) one.
	/// The driver still ends the token with [`ended`](Self::ended).
	#[cfg_attr(not(feature = "async"), allow(dead_code))]
	pub(crate) fn refused(&self, id: u64) {
		let mut state = self.state.lock();
		if state.tokens.contains_key(&id) {
			state.replied = true;
		}
	}

	/// End the token: refused or revoked by the peer, withdrawn by us, or its
	/// stream lost. A token that was never answered records `err` as its answer.
	#[cfg_attr(not(feature = "async"), allow(dead_code))]
	pub(crate) fn ended(&self, id: u64, err: Error) {
		let mut state = self.state.lock();
		let Some(slot) = state.tokens.get_mut(&id) else {
			return;
		};
		slot.grant = None;
		slot.answered.get_or_insert(Err(err.clone()));
		slot.ended = Some(err);
		// The handle is gone and the token is over: nothing will read the slot again.
		if slot.withdrawn {
			state.tokens.remove(&id);
		}
		state.recompute();
	}

	/// Resolve once every token the session presented at setup has its first
	/// reply, so enforcement never waits on a token the app added later.
	#[cfg_attr(not(feature = "async"), allow(dead_code))]
	pub(crate) fn poll_setup_answered(&self, waiter: &kio::Waiter) -> Poll<()> {
		let _ = ready_or!(self.state.poll(waiter, |state| {
			match state.tokens.values().any(|slot| slot.setup && slot.answered.is_none()) {
				true => Poll::Pending,
				false => Poll::Ready(()),
			}
		}));
		Poll::Ready(())
	}

	/// Authorize the peer's connection credential for `grant` on this session, in the
	/// session's own paths. Calling it again re-authorizes, narrower or wider, replacing the
	/// last grant. The session's origin handles still bound it, so it never grants more than
	/// they allow.
	///
	/// The session's scope for the peer is this grant and every grant issued to one of its
	/// AUTH tokens through [`requests`](Self::requests); a session never authorized is bound
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
	/// application answering tokens through [`requests`](Self::requests) updates its own
	/// [`Issued`] grants.
	pub fn authorize(&self, grant: &Grant) {
		let mut state = self.state.lock();
		if state.limit.as_ref() != Some(grant) {
			state.limit = Some(grant.clone());
			state.epoch += 1;
			state.rescope();
		}
	}

	/// Record the grant an application issued to the peer's AUTH token `id`, or forget it with
	/// `None`, resizing the scope.
	fn issue(&self, id: u64, grant: Option<Grant>) {
		let mut state = self.state.lock();
		match grant {
			Some(grant) => state.issued.insert(id, grant),
			None => state.issued.remove(&id),
		};
		state.rescope();
	}

	/// The union once it changed since `epoch` last saw it, advancing `epoch`. It
	/// only ever changes into `Some`: no answer yet is the initial state.
	#[cfg_attr(not(feature = "async"), allow(dead_code))]
	pub(crate) fn poll_union(&self, epoch: &mut u64, waiter: &kio::Waiter) -> Poll<Option<Grant>> {
		let state = ready_or!(self.poll_epoch(*epoch, waiter));
		*epoch = state.epoch;
		Poll::Ready(state.union.clone())
	}

	/// What `direction` allows once it may have changed since `epoch` last saw it,
	/// advancing `epoch`.
	#[cfg_attr(not(feature = "async"), allow(dead_code))]
	pub(crate) fn poll_permit(&self, direction: Direction, epoch: &mut u64, waiter: &kio::Waiter) -> Poll<Permit> {
		let state = ready_or!(self.poll_epoch(*epoch, waiter));
		*epoch = state.epoch;
		Poll::Ready(state.permit(direction))
	}

	/// The limit once it may have changed since `epoch` last saw it, advancing `epoch`.
	#[cfg_attr(not(feature = "async"), allow(dead_code))]
	pub(crate) fn poll_limit(&self, epoch: &mut u64, waiter: &kio::Waiter) -> Poll<Option<Grant>> {
		let state = ready_or!(self.poll_epoch(*epoch, waiter));
		*epoch = state.epoch;
		Poll::Ready(state.limit.clone())
	}

	#[cfg_attr(not(feature = "async"), allow(dead_code))]
	fn poll_epoch(&self, seen: u64, waiter: &kio::Waiter) -> Poll<kio::Mut<'_, State>> {
		self.state.poll(waiter, |state| match state.epoch != seen {
			true => Poll::Ready(()),
			false => Poll::Pending,
		})
	}

	/// Whether `direction` allows `path` right now: the union (allowing everything
	/// while it is `None`, before an answer or on a version without AUTH) and the
	/// limit both.
	#[cfg_attr(not(feature = "async"), allow(dead_code))]
	pub(crate) fn allows(&self, direction: Direction, path: &str) -> bool {
		self.state.read().allows(direction, path)
	}

	/// Whether a request token decides the request carrying it, rather than the session's
	/// scope: the application verifies every token, or the request is refused as unsupported
	/// without an acceptor, even where the scope already covers it. A token admitted unread
	/// would leave the peer believing it backs a request this side holds to its scope. A side
	/// that neither scopes the peer nor answers tokens has nothing to check one against, and
	/// admits as it would without it.
	#[cfg_attr(not(feature = "async"), allow(dead_code))]
	pub(crate) fn token_decides(&self, direction: Direction) -> bool {
		let state = self.state.read();
		let (_, scope) = state.parts(direction);
		scope.is_some() || matches!(state.acceptor, Acceptor::App(_))
	}

	/// Whether the peer's grant alone allows `direction` at `path`, allowing everything
	/// while it is `None`: what bounds this side whatever authorized the request.
	#[cfg_attr(not(feature = "async"), allow(dead_code))]
	pub(crate) fn within_grant(&self, direction: Direction, path: &str) -> bool {
		let state = self.state.read();
		let (granted, _) = state.parts(direction);
		granted.is_none_or(|granted| granted.matches(path))
	}

	/// Whether the scope alone allows `direction` at `path`: for a route the peer
	/// offers us, which our own grant does not decide.
	#[cfg_attr(not(feature = "async"), allow(dead_code))]
	pub(crate) fn within_limit(&self, direction: Direction, path: &str) -> bool {
		let state = self.state.read();
		let (_, scope) = state.parts(direction);
		scope.is_none_or(|scope| scope.matches(path))
	}

	/// The peer turned out not to negotiate AUTH: fail every token we presented as
	/// unsupported, leaving the union unknown. The peer's tokens can still ride its requests,
	/// so their queue stays open.
	#[cfg_attr(not(feature = "async"), allow(dead_code))]
	pub(crate) fn unsupported(&self) {
		let mut state = self.state.lock();
		state.supported = false;
		state.opening.clear();
		for slot in state.tokens.values_mut() {
			slot.answered.get_or_insert(Err(Error::Unsupported));
			slot.ended.get_or_insert(Error::Unsupported);
		}
		// Nothing will read a withdrawn slot again.
		state.tokens.retain(|_, slot| !slot.withdrawn);
	}

	/// End the session: fail every pending token, end every watch, and close the
	/// requests.
	#[cfg_attr(not(feature = "async"), allow(dead_code))]
	pub(crate) fn close(&self, err: Error) {
		let mut state = self.state.lock();
		if state.closed.is_some() {
			return;
		}
		state.closed = Some(err.clone());
		state.opening.clear();
		for slot in state.tokens.values_mut() {
			slot.grant = None;
			slot.answered.get_or_insert(Err(err.clone()));
			slot.ended.get_or_insert(err.clone());
		}
		state.recompute();
		let queue = match &state.acceptor {
			Acceptor::App(queue) => Some(queue.clone()),
			_ => None,
		};
		// Refusing the queued tokens takes this lock again, to drop them from the scope.
		drop(state);
		if let Some(queue) = queue {
			queue.close();
		}
	}
}

/// A token this side presented. Dropping it withdraws the token, closing its
/// stream and removing its grant from the union.
pub struct Token {
	state: kio::Shared<State>,
	id: u64,
}

impl Token {
	/// This token's own grant: `None` until the peer answers, and again once the
	/// token ends.
	pub fn grant(&self) -> Watch {
		Watch::new(self.state.clone(), Selector::Token(self.id))
	}

	/// Wait for the token to end: the peer's revocation as [`Error::Session`],
	/// [`Error::Cancel`] when the peer closed it without one, or the session's
	/// close.
	#[cfg(feature = "async")]
	pub async fn closed(&self) -> Error {
		kio::wait(|waiter| {
			let state = ready_or!(self.state.poll(waiter, |state| match state.tokens.get(&self.id) {
				Some(slot) if slot.ended.is_none() => Poll::Pending,
				_ => Poll::Ready(()),
			}));
			let err = state.tokens.get(&self.id).and_then(|slot| slot.ended.clone());
			Poll::Ready(err.unwrap_or(Error::Cancel))
		})
		.await
	}

	#[cfg_attr(not(feature = "async"), allow(dead_code))]
	fn poll_answered(&self, waiter: &kio::Waiter) -> Poll<Result<()>> {
		let state = ready_or!(self.state.poll(waiter, |state| match state.tokens.get(&self.id) {
			Some(slot) if slot.answered.is_none() => Poll::Pending,
			_ => Poll::Ready(()),
		}));
		let answered = state.tokens.get(&self.id).and_then(|slot| slot.answered.clone());
		Poll::Ready(answered.unwrap_or(Err(Error::Cancel)))
	}
}

impl Drop for Token {
	fn drop(&mut self) {
		let mut state = self.state.lock();
		let Some(slot) = state.tokens.get_mut(&self.id) else {
			return;
		};
		slot.withdrawn = true;
		// Already over, or never reached the wire: nothing left to close.
		if slot.ended.is_some() || state.opening.contains(&self.id) {
			state.opening.retain(|id| *id != self.id);
			state.tokens.remove(&self.id);
			state.recompute();
		}
	}
}

enum Selector {
	Union,
	Token(u64),
}

/// Watches a grant: the session's union, or one token's.
///
/// Created by [`Handle::grant`] and [`Token::grant`].
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

	fn current(&self, state: &State) -> Option<Grant> {
		match self.selector {
			Selector::Union => state.union.clone(),
			Selector::Token(id) => state.tokens.get(&id).and_then(|slot| slot.grant.clone()),
		}
	}

	/// Whether nothing will change the grant again.
	fn ended(&self, state: &State) -> Option<Error> {
		if let Some(err) = state.closed() {
			return Some(err);
		}
		match self.selector {
			Selector::Union => None,
			Selector::Token(id) => match state.tokens.get(&id) {
				Some(slot) => slot.ended.clone(),
				None => Some(Error::Cancel),
			},
		}
	}

	/// The grant right now.
	pub fn peek(&self) -> Option<Grant> {
		self.current(&self.state.read())
	}

	/// Poll for the grant to differ from the last one this watch returned (or saw
	/// at creation).
	///
	/// `Err` once nothing will change it again: the session closed, or for a
	/// token's watch, the token ended.
	pub fn poll_changed(&mut self, waiter: &kio::Waiter) -> Poll<Result<Option<Grant>>> {
		let last = &self.last;
		let selector = &self.selector;
		let state = ready_or!(self.state.poll(waiter, |state| {
			let current = match selector {
				Selector::Union => state.union.as_ref(),
				Selector::Token(id) => state.tokens.get(id).and_then(|slot| slot.grant.as_ref()),
			};
			let ended = state.closed.is_some()
				|| match selector {
					Selector::Union => false,
					Selector::Token(id) => state.tokens.get(id).is_none_or(|slot| slot.ended.is_some()),
				};
			match current != last.as_ref() || ended {
				true => Poll::Ready(()),
				false => Poll::Pending,
			}
		}));
		let current = self.current(&state);
		if current != self.last {
			self.last = current.clone();
			return Poll::Ready(Ok(current));
		}
		Poll::Ready(Err(self.ended(&state).unwrap_or(Error::Cancel)))
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
/// Returned by [`Handle::requests`]. Dropping it refuses every later token.
pub struct Requests {
	queue: Pending,
}

impl Requests {
	/// Poll for the next token, or `None` once the session ends.
	pub fn poll_next(&mut self, waiter: &kio::Waiter) -> Poll<Option<Request>> {
		self.queue.poll_next(waiter)
	}

	/// Wait for the next token the peer presents, or `None` once the session ends.
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
	#[cfg_attr(not(feature = "async"), allow(dead_code))]
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
	#[cfg_attr(not(feature = "async"), allow(dead_code))]
	Grant(Grant),
	#[cfg_attr(not(feature = "async"), allow(dead_code))]
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
	scoped: Option<(Handle, u64)>,
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

/// A request's slot against the session's cap, shared with the application's verification
/// of its token so work still in flight stays counted after the request ends. Opaque here:
/// auth only keeps it alive.
pub(crate) type Hold = std::sync::Arc<dyn std::any::Any + Send + Sync>;

/// The serving task's hold on one peer token's [`Issue`].
///
/// Dropping it settles [`Issued::closed`] with the session's error, unless the task
/// already recorded why the stream ended, so no exit path leaves the acceptor waiting.
#[cfg_attr(not(feature = "async"), allow(dead_code))]
pub(crate) struct Serving {
	pub issue: kio::Shared<Issue>,
	handle: Handle,
}

impl Serving {
	/// A fresh stream's shared state, settled by the session `handle` if the task drops.
	#[cfg_attr(not(feature = "async"), allow(dead_code))]
	pub(crate) fn new(handle: Handle) -> Self {
		let id = {
			let mut state = handle.state.lock();
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
	#[cfg_attr(not(feature = "async"), allow(dead_code))]
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
		let err = self.handle.state.read().closed().unwrap_or(Error::Cancel);
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
	#[cfg_attr(not(feature = "async"), allow(dead_code))]
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
#[cfg_attr(not(feature = "async"), allow(dead_code))]
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
	#[cfg_attr(not(feature = "async"), allow(dead_code))]
	pub(crate) fn covers(self, grant: &Grant, path: &str) -> bool {
		match self {
			Self::Subscribe => grant.subscribe.matches(path),
			Self::PublishNamespace => {
				crate::Pattern::subtree(path).is_ok_and(|subtree| grant.publish.contains(&subtree))
			}
		}
	}
}

/// The verdict on a request-borne token, from [`Handle::verify_request`].
///
/// Holding it keeps the request's grant alive; dropping it tells the acceptor the request
/// is over, so the grant lives exactly as long as the request and never outlives it.
pub(crate) struct RequestVerdict {
	issue: Option<kio::Shared<Issue>>,
}

/// Why a request token did not authorize its request: the error whose code the peer is
/// sent, and the reason phrase with it, which the acceptor chose when it refused.
#[derive(Debug)]
#[cfg_attr(not(feature = "async"), allow(dead_code))]
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

#[cfg_attr(not(feature = "async"), allow(dead_code))]
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
	/// enforced grant being dropped. `Pending` while the grant still stands.
	fn poll_reply(&self, waiter: &kio::Waiter) -> Poll<Reply> {
		let Some(issue) = &self.issue else {
			return Poll::Ready(Reply::Refuse {
				code: SessionError::Unauthorized,
				reason: "no verifier".to_string(),
			});
		};
		let mut guard = ready_or!(
			issue.poll(waiter, |issue| match issue.outbox.is_empty() && !issue.done {
				true => Poll::Pending,
				false => Poll::Ready(()),
			})
		);
		Poll::Ready(match guard.outbox.pop_front() {
			Some(reply) => reply,
			// Done with nothing more to read: the acceptor dropped the enforced grant, ending
			// the request.
			None => Reply::Refuse {
				code: SessionError::Unauthorized,
				reason: "grant dropped".to_string(),
			},
		})
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
#[cfg_attr(not(feature = "async"), allow(dead_code))]
pub(crate) struct RequestGrant(std::sync::Arc<std::sync::Mutex<Held>>);

#[cfg_attr(not(feature = "async"), allow(dead_code))]
struct Held {
	verdict: RequestVerdict,
	grant: Grant,
	deadline: crate::time::Deadline,
	/// The request this grant must keep covering: a replacement that no longer does ends it.
	path: crate::PathOwned,
	kind: RequestKind,
	/// The request's slot, shared with a renewal's verification.
	hold: Option<Hold>,
	clock: crate::time::Clock,
}

#[cfg_attr(not(feature = "async"), allow(dead_code))]
impl RequestGrant {
	/// Hold the acceptor's first answer `res` for the request's life, armed to lapse at its
	/// expiry, or the refusal to answer the request with: the acceptor's own, or
	/// [`Error::Unauthorized`] for a grant that does not cover it or has already expired.
	pub(crate) fn new(
		clock: &crate::time::Clock,
		verdict: RequestVerdict,
		res: std::result::Result<Grant, Refusal>,
		path: crate::PathOwned,
		kind: RequestKind,
		hold: Option<Hold>,
	) -> std::result::Result<Self, Refusal> {
		let grant = res?;
		if !live(clock, kind, path.as_str(), &grant) {
			return Err(Refusal::new(Error::Unauthorized, "request token not granted"));
		}
		let mut deadline = crate::time::Deadline::new(clock);
		deadline.set(grant.expires);
		Ok(Self(std::sync::Arc::new(std::sync::Mutex::new(Held {
			verdict,
			grant,
			deadline,
			path,
			kind,
			hold,
			clock: clock.clone(),
		}))))
	}

	fn held(&self) -> std::sync::MutexGuard<'_, Held> {
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
	pub(crate) fn verify_renewal(&self, handle: &Handle, token: crate::setup::Token) -> RequestVerdict {
		let held = self.held();
		handle.verify_request(token, held.hold.clone())
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
		let grant = res?;
		let mut held = self.held();
		if !live(&held.clock, held.kind, held.path.as_str(), &grant) {
			return Err(Refusal::new(Error::Unauthorized, "renewal not granted"));
		}
		held.verdict = verdict;
		held.deadline.set(grant.expires);
		held.grant = grant;
		Ok(())
	}

	/// Resolve with the refusal that ends the request: the deadline lapsing
	/// ([`Error::Expired`]), the acceptor revoking or dropping the grant (its code and
	/// reason), or an acceptor-side update that no longer covers the request
	/// ([`Error::Unauthorized`]). A covering update, such as a lowered expiry, is folded in and
	/// polling continues. Never resolves while the grant still stands.
	pub(crate) fn poll_ended(&self, waiter: &kio::Waiter) -> Poll<Refusal> {
		let mut held = self.held();
		loop {
			if held.deadline.poll(waiter).is_ready() {
				return Poll::Ready(Refusal::new(Error::Expired, "request token expired"));
			}
			match held.verdict.poll_reply(waiter) {
				Poll::Ready(Reply::Grant(grant)) => {
					if !held.kind.covers(&grant, held.path.as_str()) {
						return Poll::Ready(Refusal::new(Error::Unauthorized, "request token no longer granted"));
					}
					held.deadline.set(grant.expires);
					held.grant = grant;
					// Re-poll: the new deadline may already have lapsed, or another reply may
					// be waiting.
					continue;
				}
				Poll::Ready(Reply::Refuse { code, reason }) => {
					return Poll::Ready(Refusal::new(Error::Session(code), &reason));
				}
				Poll::Pending => return Poll::Pending,
			}
		}
	}

	/// Run `body`, everything a request does once admitted, until it finishes or `grant`
	/// (when its token admitted it) ends. See [`until`].
	pub(crate) fn guard<F: std::future::Future + kio::MaybeSend>(
		grant: Option<&Self>,
		body: F,
	) -> impl std::future::Future<Output = std::result::Result<F::Output, Refusal>> + kio::MaybeSend {
		crate::util::sendable(until(body, move |waiter| match grant {
			Some(grant) => grant.poll_ended(waiter),
			None => Poll::Pending,
		}))
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
#[cfg_attr(not(feature = "async"), allow(dead_code))]
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
#[cfg_attr(not(feature = "async"), allow(dead_code))]
fn live(clock: &crate::time::Clock, kind: RequestKind, path: &str, grant: &Grant) -> bool {
	kind.covers(grant, path) && grant.expires.is_none_or(|expires| expires > clock.now())
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
#[cfg_attr(not(feature = "async"), allow(dead_code))]
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
#[cfg_attr(not(feature = "async"), allow(dead_code))]
pub(crate) struct Gate {
	handle: Handle,
	path: crate::PathOwned,
	direction: Direction,
	epoch: u64,
	part: Part,
	/// Whether the scope allowed the path at the last permit.
	within_limit: bool,
}

/// Which part of a [`Permit`] a [`Gate`] watches.
#[cfg_attr(not(feature = "async"), allow(dead_code))]
enum Part {
	/// The peer's grant and the scope both.
	Both,
	/// The peer's grant alone: a token the peer presented stands in for the scope.
	Granted,
	/// The peer's grant, and the scope once the announce that stood in for it closes.
	Announced(kio::Consumer<()>),
}

impl Gate {
	#[cfg_attr(not(feature = "async"), allow(dead_code))]
	pub(crate) fn new(handle: Handle, path: crate::PathOwned, direction: Direction) -> Self {
		let within_limit = handle.within_limit(direction, path.as_str());
		Self {
			handle,
			path,
			direction,
			epoch: 0,
			part: Part::Both,
			within_limit,
		}
	}

	/// A gate on the peer's grant alone, for a peer's request its own token authorized.
	#[cfg_attr(not(feature = "async"), allow(dead_code))]
	pub(crate) fn granted(handle: Handle, path: crate::PathOwned, direction: Direction) -> Self {
		Self {
			part: Part::Granted,
			..Self::new(handle, path, direction)
		}
	}

	/// A gate for a request under a namespace the peer announced, while `live` stays open:
	/// the announce stands in for the scope, as its own token may have authorized it.
	#[cfg_attr(not(feature = "async"), allow(dead_code))]
	pub(crate) fn announced(
		handle: Handle,
		path: crate::PathOwned,
		direction: Direction,
		live: kio::Consumer<()>,
	) -> Self {
		Self {
			part: Part::Announced(live),
			..Self::new(handle, path, direction)
		}
	}

	/// Whether the session allows the path right now.
	#[cfg_attr(not(feature = "async"), allow(dead_code))]
	pub(crate) fn allows(&self) -> bool {
		let path = self.path.as_str();
		self.handle.within_grant(self.direction, path)
			&& match &self.part {
				Part::Both => self.within_limit,
				Part::Granted => true,
				Part::Announced(live) => self.within_limit || !live.is_closed(),
			}
	}

	/// Resolve once the session no longer allows the path. A union that is still
	/// `None` (no answer yet, or a version without AUTH) and a session never limited
	/// allow everything.
	#[cfg_attr(not(feature = "async"), allow(dead_code))]
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
#[cfg_attr(not(feature = "async"), allow(dead_code))]
pub(crate) struct DefaultGrant {
	handle: Handle,
	base: Grant,
	epoch: u64,
	limit: Option<Grant>,
	/// The grant last handed out, so an unrelated wakeup sends nothing.
	sent: Option<Grant>,
}

impl DefaultGrant {
	#[cfg_attr(not(feature = "async"), allow(dead_code))]
	pub(crate) fn new(handle: Handle, base: Grant) -> Self {
		let limit = handle.state.read().limit.clone();
		Self {
			handle,
			base,
			epoch: 0,
			limit,
			sent: None,
		}
	}

	/// The grant to send next: the first, then each one a new limit changes.
	#[cfg_attr(not(feature = "async"), allow(dead_code))]
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
#[cfg_attr(not(feature = "async"), allow(dead_code))]
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
#[cfg_attr(not(feature = "async"), allow(dead_code))]
pub(crate) struct Enforce {
	epoch: u64,
	permit: Option<Patterns>,
	/// Every broadcast admitted so far, still announced.
	live: std::collections::HashSet<crate::PathOwned>,
	setup: bool,
}

impl Enforce {
	/// Resolve with the first broadcast announced outside the union, or `None` once the
	/// origin ends.
	#[cfg_attr(not(feature = "async"), allow(dead_code))]
	pub(crate) fn poll(
		&mut self,
		handle: &Handle,
		announced: &mut crate::announce::Consumer,
		waiter: &kio::Waiter,
	) -> Poll<Option<crate::PathOwned>> {
		if !self.setup {
			ready_or!(handle.poll_setup_answered(waiter));
			self.setup = true;
		}
		while let Poll::Ready(union) = handle.poll_union(&mut self.epoch, waiter) {
			self.permit = union.map(|grant| grant.publish);
		}
		// No grant yet (the peer never answered with one): nothing to check against.
		let Some(permit) = &self.permit else {
			return Poll::Pending;
		};

		loop {
			let Some(event) = ready_or!(announced.poll_next(waiter)) else {
				return Poll::Ready(None);
			};
			match event {
				crate::announce::Event::Start(update) | crate::announce::Event::Update(update)
					if !self.live.contains(&update.prefix) =>
				{
					if !permit.matches(update.prefix.as_str()) {
						return Poll::Ready(Some(update.prefix));
					}
					self.live.insert(update.prefix);
				}
				crate::announce::Event::End(update) => {
					self.live.remove(&update.prefix);
				}
				_ => {}
			}
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	/// A queued request token keeps its request's slot, so the peer's request credit bounds
	/// how many wait; answering it gives the slot back.
	#[test]
	fn a_queued_request_token_holds_its_slot() {
		let handle = Handle::new(true);
		let mut requests = handle.requests().unwrap();
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
		let handle = Handle::new(true);
		let mut requests = handle.requests().unwrap();
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
		let handle = Handle::new(false);
		assert!(handle.allows(Direction::Publish, "room/alice/audio"));

		handle.authorize(&grant(&["room/bob"], &["room/alice"]));
		// What we send is what the peer may subscribe to, and the other way around.
		assert!(handle.allows(Direction::Publish, "room/alice/audio"));
		assert!(!handle.allows(Direction::Publish, "room/bob/cam"));
		assert!(handle.allows(Direction::Subscribe, "room/bob/cam"));
		assert!(!handle.allows(Direction::Subscribe, "room/alice/audio"));

		handle.authorize(&grant(&[""], &["room/alice/video"]));
		assert!(!handle.allows(Direction::Publish, "room/alice/audio"));
		assert!(handle.allows(Direction::Publish, "room/alice/video"));
		assert!(handle.allows(Direction::Subscribe, "other"));

		handle.authorize(&Grant::all());
		assert!(handle.allows(Direction::Publish, "room/alice/audio"));
		assert!(handle.allows(Direction::Subscribe, "other"));
	}

	/// Our own grant does not decide what the peer may offer us, only the limit does.
	#[test]
	fn the_limit_alone_decides_what_the_peer_offers() {
		let handle = Handle::new(true);
		let token = handle.present(Bytes::new(), true).unwrap();
		handle.granted(0, grant(&[], &[]));
		assert!(!handle.allows(Direction::Subscribe, "room/bob/cam"));
		assert!(handle.within_limit(Direction::Subscribe, "room/bob/cam"));

		handle.authorize(&grant(&["room/alice"], &[]));
		assert!(!handle.within_limit(Direction::Subscribe, "room/bob/cam"));
		assert!(handle.within_limit(Direction::Subscribe, "room/alice/cam"));
		drop(token);
	}

	/// The default acceptor answers with its base grant, then with each grant a new
	/// limit changes, narrower or wider, and stays quiet on a change that leaves it alone.
	#[test]
	fn the_default_grant_follows_the_limit() {
		let handle = Handle::new(true);
		let mut default = DefaultGrant::new(handle.clone(), grant(&["room"], &["room"]));
		let waiter = kio::Waiter::noop();

		assert_eq!(default.poll(&waiter), Poll::Ready(grant(&["room"], &["room"])));
		assert!(default.poll(&waiter).is_pending());

		handle.authorize(&grant(&["room"], &["room/alice"]));
		assert_eq!(default.poll(&waiter), Poll::Ready(grant(&["room"], &["room/alice"])));

		// Wider again, but never past what the origin handles allow.
		handle.authorize(&Grant::all());
		assert_eq!(default.poll(&waiter), Poll::Ready(grant(&["room"], &["room"])));

		// A limit that leaves the grant as it is: nothing to tell the peer.
		handle.authorize(&grant(&[""], &[""]));
		assert!(default.poll(&waiter).is_pending());
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

	fn subscribe_path() -> crate::PathOwned {
		crate::Path::new("room/alice").to_owned()
	}

	/// A request token reaches the app's acceptor like any other token, and the grant the
	/// app answers is what the verdict resolves to.
	#[moq_net_sim::test]
	async fn a_request_token_reaches_the_acceptor() {
		let handle = Handle::new(true);
		let mut requests = handle.requests().expect("take the requests");
		let verdict = handle.verify_request(token(b"jwt", 0), None);

		let request = requests.next().await.expect("a request");
		assert_eq!(request.token().as_ref(), b"jwt");
		let _issued = request.accept(Grant::all());

		let grant = verdict.grant().await.expect("granted");
		assert!(grant.publish.matches("room/alice"));
	}

	/// An AUTH token has no type, so a request token of any type but out-of-band cannot reach
	/// the acceptor as one: it is unsupported, never handed over as if it were a plain token.
	#[moq_net_sim::test]
	async fn a_typed_request_token_is_unsupported() {
		let handle = Handle::new(true);
		let mut requests = handle.requests().unwrap();
		let verdict = handle.verify_request(token(b"cat", crate::setup::Token::CAT), None);
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
		let handle = Handle::new(true);
		let mut requests = handle.requests().unwrap();
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
	#[moq_net_sim::test]
	async fn a_request_token_is_verified_without_the_auth_extension() {
		let handle = Handle::new(false);
		let mut requests = handle.requests().expect("take the requests");
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
	#[moq_net_sim::test]
	async fn no_consumer_refuses_a_request_token_as_unsupported() {
		let handle = Handle::new(true);
		let verdict = handle.verify_request(token(b"jwt", 0), None);
		let err = verdict.grant().await.expect_err("no verifier");
		assert!(matches!(err.error, Error::Unsupported), "{err:?}");
	}

	/// A refused request token resolves to the refusal, which the caller sends the peer as
	/// UNAUTHORIZED.
	#[moq_net_sim::test]
	async fn a_refused_request_token_is_unauthorized() {
		let handle = Handle::new(true);
		let mut requests = handle.requests().unwrap();
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
	#[moq_net_sim::test]
	async fn a_grant_ended_before_admission_is_refused() {
		let handle = Handle::new(true);
		let mut requests = handle.requests().unwrap();

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
	#[moq_net_sim::test]
	async fn dropping_the_verdict_ends_the_grant() {
		let handle = Handle::new(true);
		let mut requests = handle.requests().unwrap();
		let verdict = handle.verify_request(token(b"jwt", 0), None);
		let issued = requests.next().await.unwrap().accept(Grant::all());
		verdict.grant().await.expect("granted");
		drop(verdict);
		let err = issued.closed().await;
		assert!(matches!(err, Error::Cancel), "{err:?}");
	}

	/// A request token never joins the session union: the union stays what the connection
	/// credential earned, not what a request token granted.
	#[moq_net_sim::test]
	async fn a_request_token_never_joins_the_union() {
		let handle = Handle::new(true);
		let mut requests = handle.requests().unwrap();
		let verdict = handle.verify_request(token(b"jwt", 0), None);
		let _issued = requests.next().await.unwrap().accept(Grant::all());
		verdict.grant().await.expect("granted");
		// The union only reflects tokens presented on this side (none here), never a
		// request token the acceptor answered.
		assert_eq!(handle.grant().peek(), None, "a request token must not widen the union");
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
	#[moq_net_sim::test]
	async fn the_scope_is_session_tokens_only() {
		let handle = Handle::new(true);
		let mut requests = handle.requests().unwrap();
		handle.authorize(&subscribe_only("room/alice"));
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
	#[moq_net_sim::test]
	async fn an_ended_auth_grant_leaves_the_scope() {
		let handle = Handle::new(true);
		let mut requests = handle.requests().unwrap();
		handle.authorize(&Grant::default());
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
	#[moq_net_sim::test]
	async fn an_unauthorized_session_has_no_scope() {
		let handle = Handle::new(true);
		let mut requests = handle.requests().unwrap();
		let serving = Serving::new(handle.clone());
		requests
			.queue
			.push(Request::new(Bytes::from_static(b"auth"), serving.issue.clone()));
		let _issued = requests.next().await.unwrap().accept(subscribe_only("room"));
		assert!(handle.allows(Direction::Publish, "other"));
	}

	use std::time::Duration;

	fn grant_in(runtime: &crate::time::Clock, secs: u64) -> Grant {
		Grant {
			publish: Patterns::new(),
			subscribe: crate::Pattern::all().into(),
			expires: runtime.now().checked_add(Duration::from_secs(secs)),
		}
	}

	/// The request grant carries the acceptor's expiry, which is the deadline it lapses at.
	#[moq_net_sim::test]
	async fn a_request_grant_has_the_acceptors_expiry() {
		let runtime = crate::time::Clock::sim();
		let handle = Handle::new(true);
		let mut requests = handle.requests().unwrap();
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
	#[moq_net_sim::test]
	async fn an_already_expired_grant_is_refused() {
		let runtime = crate::time::Clock::sim();
		let handle = Handle::new(true);
		let mut requests = handle.requests().unwrap();
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
	#[moq_net_sim::test]
	async fn a_renewal_token_replaces_the_grant_and_expiry() {
		let runtime = crate::time::Clock::sim();
		let handle = Handle::new(true);
		let mut requests = handle.requests().unwrap();

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
	#[moq_net_sim::test]
	async fn the_deadline_expires_the_request_without_renewal() {
		let runtime = crate::time::Clock::sim();
		let handle = Handle::new(true);
		let mut requests = handle.requests().unwrap();
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
		assert_eq!(handle.grant().peek(), None, "the session grant is untouched");
	}

	/// A refused renewal verdict is not applied: the request grant it already holds stands
	/// untouched and ends only when its own deadline lapses. The serve loops end the request on
	/// a refusal separately (a refused renewal ends only that request); this covers only that
	/// the grant object is not mutated.
	#[moq_net_sim::test]
	async fn a_refused_renewal_leaves_the_grant() {
		let runtime = crate::time::Clock::sim();
		let handle = Handle::new(true);
		let mut requests = handle.requests().unwrap();
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
	#[moq_net_sim::test]
	async fn an_update_that_stops_covering_ends_the_request() {
		let runtime = crate::time::Clock::sim();
		let handle = Handle::new(true);
		let mut requests = handle.requests().unwrap();
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
		let err = moq_net_sim::timeout(
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
	#[moq_net_sim::test]
	async fn an_acceptor_revoke_ends_the_request() {
		let runtime = crate::time::Clock::sim();
		let handle = Handle::new(true);
		let mut requests = handle.requests().unwrap();
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
		assert_eq!(handle.grant().peek(), None, "the session grant is untouched");
	}
}
