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
//! moq-transport draft-17+ carries the same exchange when both sides negotiate the
//! MoQ Auth extension. Older versions, and peers that do not negotiate it, carry no
//! AUTH exchange: there the grant stays `None` and `add` fails with
//! [`Error::Unsupported`]. moq-lite carries a grant's patterns as they are;
//! moq-transport carries namespace prefixes, so it refuses a grant that is not a
//! union of subtrees rather than widen it.

use std::{
	collections::{BTreeMap, HashMap, VecDeque},
	sync::{Arc, Mutex},
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
	/// The session speaks moq-lite, whose requests carry no token.
	lite: bool,
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
	/// The most this side lets the peer do, in the peer's terms: `None` (only the
	/// origin handles bound it) until [`Handle::authorize`] sets it; the grant it was
	/// last called with.
	limit: Option<Grant>,
	/// Bumped whenever the union or the limit changes, so the session's per-stream
	/// gates can skip re-matching paths on every wakeup.
	epoch: u64,
	acceptor: Acceptor,
	/// The application's queue of the peer's request tokens, once it took [`Handle::scoped`].
	verifier: Option<Pending<Scoped>>,
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
	App(Pending<Request>),
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

	/// The error handed to anything waiting on a session that has ended.
	fn closed(&self) -> Option<Error> {
		self.closed.clone()
	}

	/// The union's and the limit's patterns for `direction`, each `None` while
	/// unrestricted.
	#[cfg_attr(not(feature = "async"), allow(dead_code))]
	fn parts(&self, direction: Direction) -> (Option<&Patterns>, Option<&Patterns>) {
		// The peer's grant names what we may do; the limit names what the peer may,
		// so what we send is what the peer may subscribe to, and the other way around.
		match direction {
			Direction::Publish => (
				self.union.as_ref().map(|union| &union.publish),
				self.limit.as_ref().map(|limit| &limit.subscribe),
			),
			Direction::Subscribe => (
				self.union.as_ref().map(|union| &union.subscribe),
				self.limit.as_ref().map(|limit| &limit.publish),
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

/// The `AUTHORIZATION TOKEN` this side presents on its own SUBSCRIBE and PUBLISH_NAMESPACE
/// requests (MoQ request-token), shared between a session's [`Handle`] and its driver.
///
/// The session reads the current value when it first sends a request, and re-presents a
/// changed one on each live request as a REQUEST_UPDATE. The default presents no token,
/// which is byte-identical to a session that never sets one.
#[derive(Clone, Default)]
#[cfg_attr(not(feature = "async"), allow(dead_code))]
pub(crate) struct RequestToken {
	token: kio::Shared<Option<crate::setup::Token>>,
	/// Broadcasts whose live advertisement presented the token, counted per advertisement.
	/// They keep standing on it after it is cleared, so grant enforcement leaves them be.
	backed: Arc<Mutex<HashMap<crate::PathOwned, usize>>>,
}

#[cfg_attr(not(feature = "async"), allow(dead_code))]
impl RequestToken {
	/// A credential presenting `token` (or none) until replaced.
	#[cfg(test)]
	pub(crate) fn new(token: Option<crate::setup::Token>) -> Self {
		Self {
			token: kio::Shared::new(token),
			backed: Default::default(),
		}
	}

	/// The token an advertisement of `path` presents now, with a guard marking `path` as backed
	/// by it until dropped. Read under the token's lock, so a concurrent clear sees either no
	/// token sent or the path already backed.
	pub(crate) fn present(&self, path: &crate::PathOwned) -> (Option<crate::setup::Token>, Option<Backing>) {
		let token = self.token.read();
		let backing = token.is_some().then(|| self.back(path.clone()));
		((*token).clone(), backing)
	}

	/// Mark `path` as backed by the token, such as after an accepted renewal.
	pub(crate) fn back(&self, path: crate::PathOwned) -> Backing {
		*self
			.backed
			.lock()
			.unwrap_or_else(|e| e.into_inner())
			.entry(path.clone())
			.or_default() += 1;
		Backing {
			backed: self.backed.clone(),
			path,
		}
	}

	/// Whether a live advertisement of `path` presented the token.
	pub(crate) fn backs(&self, path: &crate::PathOwned) -> bool {
		self.backed.lock().unwrap_or_else(|e| e.into_inner()).contains_key(path)
	}

	/// Replace the token presented on this session's requests. A live request re-presents
	/// it as a REQUEST_UPDATE on its next turn; setting the same value again is a no-op.
	pub(crate) fn set(&self, token: Option<crate::setup::Token>) {
		*self.token.lock() = token;
	}

	/// The token to present right now, read when a request is first sent.
	pub(crate) fn peek(&self) -> Option<crate::setup::Token> {
		self.token.read().clone()
	}

	/// Ready with the current token once it differs from `last`, registering `waiter`
	/// otherwise. The send loops park here to re-present a replaced token on their live
	/// requests; it reads without advancing `last`, so a poll that loses its turn to
	/// another arm is re-offered the change rather than dropping it.
	pub(crate) fn poll_changed(
		&self,
		last: &Option<crate::setup::Token>,
		waiter: &kio::Waiter,
	) -> Poll<Option<crate::setup::Token>> {
		let guard = ready_or!(self.token.poll(waiter, |cur| match **cur == *last {
			true => Poll::Pending,
			false => Poll::Ready(()),
		}));
		Poll::Ready((*guard).clone())
	}
}

/// One advertisement standing on the request token it presented; dropping it ends that.
#[cfg_attr(not(feature = "async"), allow(dead_code))]
pub(crate) struct Backing {
	backed: Arc<Mutex<HashMap<crate::PathOwned, usize>>>,
	path: crate::PathOwned,
}

impl Drop for Backing {
	fn drop(&mut self) {
		let mut backed = self.backed.lock().unwrap_or_else(|e| e.into_inner());
		if let Some(count) = backed.get_mut(&self.path) {
			*count -= 1;
			if *count == 0 {
				backed.remove(&self.path);
			}
		}
	}
}

/// The session's auth handle, returned by `Session::auth`.
///
/// Cheap to clone; every clone shares the session's tokens.
#[derive(Clone)]
pub struct Handle {
	state: kio::Shared<State>,
	request_token: RequestToken,
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
			request_token: RequestToken::default(),
		}
	}

	/// Present `token` as the `AUTHORIZATION TOKEN` on this side's own SUBSCRIBE and
	/// PUBLISH_NAMESPACE requests (MoQ request-token), or stop presenting one with `None`.
	/// Unlike `add`, which presents a credential for the whole session, the
	/// peer verifies this one per request. Experimental.
	///
	/// Set it before running the session's driver to present it on the first request.
	/// Replacing it later re-presents the new token on every live request as a
	/// REQUEST_UPDATE, renewing a token-authorized request in place; setting the same value
	/// again sends nothing. Only drafts 14 and 15 cannot renew an advertisement, which has
	/// no update there, so it keeps the token it was offered with. `None` sends nothing
	/// either: live requests keep the token they last presented, and later ones carry none.
	/// Independent of the MoQ Auth extension.
	///
	/// While a token is set, this side stops closing the session over a broadcast its own
	/// grant does not cover, since each announce carries the token for the peer to verify.
	/// Clearing it resumes that check for broadcasts announced afterwards.
	///
	/// # Errors
	///
	/// [`Error::Unsupported`] for a token on a moq-lite session, whose requests cannot carry
	/// one, rather than drop it.
	pub fn set_request_token(&self, token: Option<crate::setup::Token>) -> Result<()> {
		if token.is_some() && self.state.read().lite {
			return Err(Error::Unsupported);
		}
		self.request_token.set(token);
		Ok(())
	}

	/// The session speaks moq-lite, so [`set_request_token`](Self::set_request_token) and
	/// [`scoped`](Self::scoped) refuse.
	#[cfg_attr(not(feature = "async"), allow(dead_code))]
	pub(crate) fn lite(&self) {
		self.state.lock().lite = true;
	}

	/// The request-token cell the session's publisher and subscriber present from.
	#[cfg_attr(not(feature = "async"), allow(dead_code))]
	pub(crate) fn request_token(&self) -> RequestToken {
		self.request_token.clone()
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

	/// Answer every session token the peer presents, instead of the default acceptor.
	/// A token on one of the peer's requests goes to [`scoped`](Self::scoped) instead.
	///
	/// Call it before running the session's driver: whoever answers is decided
	/// once, on the driver's first poll, so a later call cannot take over tokens
	/// already in flight. Without it the session grants the peer what its own
	/// origin handles allow for the connection's credential, and refuses any other
	/// token as unsupported.
	///
	/// At most 128 unanswered tokens queue, not counting withdrawn ones; more are
	/// refused with [`SessionError::TooManyRequests`] (`UNAUTHORIZED` on moq-transport,
	/// whose AUTH_ERROR registry has no such code).
	///
	/// # Errors
	///
	/// [`Error::Duplicate`] if the requests were already taken, or the driver
	/// already started answering them itself.
	pub fn requests(&self) -> Result<Requests<Request>> {
		let mut state = self.state.lock();
		if !matches!(state.acceptor, Acceptor::Undecided) {
			return Err(Error::Duplicate);
		}
		let queue = Pending::default();
		// A version without AUTH never receives a token, so its requests end with
		// the session like any other.
		if !state.supported {
			queue.close();
		}
		state.acceptor = Acceptor::App(queue.clone());
		Ok(Requests { queue })
	}

	/// Verify the `AUTHORIZATION TOKEN` on each of the peer's SUBSCRIBE and PUBLISH_NAMESPACE
	/// requests that [`authorize`](Self::authorize) does not cover. Experimental.
	///
	/// Each token decides its request alone, with a grant that never joins the session's.
	/// Without this, such a request is refused as unsupported, and a session never
	/// authorized admits every request without reading its token. Take it before running the
	/// session's driver, so no request is admitted before it. Independent of the MoQ Auth
	/// extension, since the token rides the request itself.
	///
	/// At most 128 unanswered tokens queue, not counting withdrawn requests; more are
	/// refused with [`SessionError::TooManyRequests`].
	///
	/// # Errors
	///
	/// [`Error::Duplicate`] if already taken, or [`Error::Unsupported`] on moq-lite, whose
	/// requests carry no token.
	pub fn scoped(&self) -> Result<Requests<Scoped>> {
		let mut state = self.state.lock();
		if state.lite {
			return Err(Error::Unsupported);
		}
		if state.verifier.is_some() {
			return Err(Error::Duplicate);
		}
		let queue = Pending::default();
		if state.closed.is_some() {
			queue.close();
		}
		state.verifier = Some(queue.clone());
		Ok(Requests { queue })
	}

	/// Ask the application to verify a token that rode on one request.
	///
	/// The token reaches it through [`scoped`](Self::scoped). The grant it answers covers only
	/// this request, never joins the session union, and ends when the returned
	/// [`RequestVerdict`] is dropped. Without a verifier the token cannot be verified, so the
	/// verdict is [`Error::Unsupported`]. `hold` is the request's slot, kept while the
	/// application verifies the token.
	#[cfg_attr(not(feature = "async"), allow(dead_code))]
	pub(crate) fn verify_request(
		&self,
		token: crate::setup::Token,
		path: crate::PathOwned,
		kind: RequestKind,
		hold: Option<Hold>,
	) -> RequestVerdict {
		let Some(queue) = self.state.read().verifier.clone() else {
			return RequestVerdict { issue: None };
		};
		let issue = kio::Shared::new(Issue {
			hold,
			..Default::default()
		});
		// A closed or full queue refuses the token.
		queue.push(Scoped {
			token,
			path,
			kind,
			issue: Some(issue.clone()),
		});
		RequestVerdict { issue: Some(issue) }
	}

	/// Decide who answers the peer's tokens: the app if it took the requests,
	/// otherwise the session itself (`None`). Idempotent.
	#[cfg_attr(not(feature = "async"), allow(dead_code))]
	pub(crate) fn acceptor(&self) -> Option<Pending<Request>> {
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

	/// Authorize the peer for `grant` on this session, in the session's own paths.
	/// Calling it again re-authorizes, narrower or wider, replacing the last grant. The
	/// session's origin handles still bound it, so it never grants more than they allow.
	///
	/// A request outside it that carries its own token is decided by that token through
	/// [`scoped`](Self::scoped), or refused as unsupported when nothing verifies them.
	///
	/// A narrower grant ends what falls outside at once, with [`Error::Unauthorized`]
	/// where it has a reader: announcements to the peer retract, its new requests are
	/// refused, its subscriptions reset, and the broadcasts it published abort. A wider
	/// one brings back what the old grant held back: announcements to the peer are made
	/// again, and so are the peer's announcements it withheld from the origin, and new
	/// requests are accepted. The rest of the session carries on. It works on every
	/// version, since the session enforces it without the peer's help.
	///
	/// When the session answers the peer's connection credential itself, the peer is
	/// told the grant, within what the origin handles allow, `expires` included. An application answering
	/// tokens through [`requests`](Self::requests) updates its own [`Issued`] grants.
	pub fn authorize(&self, grant: &Grant) {
		let mut state = self.state.lock();
		if state.limit.as_ref() != Some(grant) {
			state.limit = Some(grant.clone());
			state.epoch += 1;
		}
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

	/// Whether a request token decides a request at `path`, rather than this side's own
	/// authorization of the peer: the limit does not cover it, so the application verifies the
	/// token, or the request is refused as unsupported without a verifier. A session never
	/// limited asks only an application that verifies request tokens, and admits otherwise.
	#[cfg_attr(not(feature = "async"), allow(dead_code))]
	pub(crate) fn token_decides(&self, direction: Direction, path: &str) -> bool {
		let state = self.state.read();
		let (_, limit) = state.parts(direction);
		match limit {
			Some(limit) => !limit.matches(path),
			None => state.verifier.is_some(),
		}
	}

	/// Whether the peer's grant alone allows `direction` at `path`, allowing everything
	/// while it is `None`: what bounds this side whatever authorized the request.
	#[cfg_attr(not(feature = "async"), allow(dead_code))]
	pub(crate) fn within_grant(&self, direction: Direction, path: &str) -> bool {
		let state = self.state.read();
		let (granted, _) = state.parts(direction);
		granted.is_none_or(|granted| granted.matches(path))
	}

	/// Whether the limit alone allows `direction` at `path`: for a route the peer
	/// offers us, which our own grant does not decide.
	#[cfg_attr(not(feature = "async"), allow(dead_code))]
	pub(crate) fn within_limit(&self, direction: Direction, path: &str) -> bool {
		let state = self.state.read();
		let (_, limit) = state.parts(direction);
		limit.is_none_or(|limit| limit.matches(path))
	}

	/// The peer turned out not to negotiate AUTH: fail every token as unsupported and
	/// close the requests, leaving the union unknown.
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
		// Request tokens ride the requests themselves, so their queue stays open.
		if let Acceptor::App(queue) = &state.acceptor {
			queue.close();
		}
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
		if let Acceptor::App(queue) = &state.acceptor {
			queue.close();
		}
		if let Some(queue) = &state.verifier {
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

/// The tokens the peer presents, for an application that answers them itself: session
/// tokens ([`Request`]) from [`Handle::requests`], and tokens on single requests
/// ([`Scoped`]) from [`Handle::scoped`].
///
/// Dropping it refuses every later token.
pub struct Requests<T = Request> {
	queue: Pending<T>,
}

impl Requests<Request> {
	/// Poll for the next session token, or `None` once the session ends.
	pub fn poll_next(&mut self, waiter: &kio::Waiter) -> Poll<Option<Request>> {
		self.queue.poll_next(waiter)
	}

	/// Wait for the next session token the peer presents, or `None` once the session ends.
	#[cfg(feature = "async")]
	pub async fn next(&mut self) -> Option<Request> {
		kio::wait(|waiter| self.poll_next(waiter)).await
	}
}

impl Requests<Scoped> {
	/// Poll for the next request token, or `None` once the session ends.
	pub fn poll_next(&mut self, waiter: &kio::Waiter) -> Poll<Option<Scoped>> {
		self.queue.poll_next(waiter)
	}

	/// Wait for the next request token the peer presents, or `None` once the session ends.
	#[cfg(feature = "async")]
	pub async fn next(&mut self) -> Option<Scoped> {
		kio::wait(|waiter| self.poll_next(waiter)).await
	}
}

impl<T> Drop for Requests<T> {
	fn drop(&mut self) {
		self.queue.close();
	}
}

/// A token waiting in a [`Pending`] queue for the application's answer.
pub(crate) trait Queued {
	/// What the session and the application share about it.
	fn issue(&self) -> &kio::Shared<Issue>;
	/// Refuse it without asking the application.
	fn refuse(self, code: SessionError, reason: &str);
}

impl Queued for Request {
	fn issue(&self) -> &kio::Shared<Issue> {
		self.issue.as_ref().expect("unanswered request")
	}

	fn refuse(self, code: SessionError, reason: &str) {
		self.reject(code, reason)
	}
}

impl Queued for Scoped {
	fn issue(&self) -> &kio::Shared<Issue> {
		self.issue.as_ref().expect("unanswered request")
	}

	fn refuse(self, code: SessionError, reason: &str) {
		self.reject(code, reason)
	}
}

/// Pending application decisions, bounded independently of transport stream credit.
pub(crate) struct Pending<T> {
	state: kio::Shared<PendingState<T>>,
}

impl<T> Clone for Pending<T> {
	fn clone(&self) -> Self {
		Self {
			state: self.state.clone(),
		}
	}
}

impl<T> Default for Pending<T> {
	fn default() -> Self {
		Self {
			state: kio::Shared::new(PendingState {
				requests: VecDeque::new(),
				closed: false,
			}),
		}
	}
}

struct PendingState<T> {
	requests: VecDeque<T>,
	closed: bool,
}

impl<T> Pending<T> {
	fn close(&self) {
		let mut state = self.state.lock();
		state.closed = true;
		state.requests.clear();
	}
}

impl<T: Queued> Pending<T> {
	/// Queue the token for the application, or refuse it when closed or full.
	#[cfg_attr(not(feature = "async"), allow(dead_code))]
	pub(crate) fn push(&self, request: T) {
		let mut state = self.state.lock();
		// Dropping the request refuses it as unanswered.
		if state.closed {
			return;
		}
		// A withdrawn stream must not occupy a slot while the verifier is busy.
		if state.requests.len() >= 128 {
			state.requests.retain(|request| request.issue().read().peer.is_none());
		}
		if state.requests.len() >= 128 {
			drop(state);
			request.refuse(SessionError::TooManyRequests, "too many pending tokens");
			return;
		}
		state.requests.push_back(request);
	}

	fn poll_next(&self, waiter: &kio::Waiter) -> Poll<Option<T>> {
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
	/// The application took the request token off the queue.
	taken: bool,
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
		Self {
			issue: kio::Shared::default(),
			handle,
		}
	}

	/// Record why the stream ended on the peer's side; the first reason wins.
	#[cfg_attr(not(feature = "async"), allow(dead_code))]
	pub(crate) fn end(&self, err: Error) {
		self.issue.lock().peer.get_or_insert(err);
	}
}

impl Drop for Serving {
	fn drop(&mut self) {
		let err = self.handle.state.read().closed().unwrap_or(Error::Cancel);
		self.end(err);
	}
}

/// A token for the whole session, waiting for an answer.
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

	/// The token the peer presented. Empty means the credential its connection
	/// already carried, or none.
	pub fn token(&self) -> &Bytes {
		&self.token
	}

	/// Wait for the peer to withdraw this unanswered token or close the session.
	#[cfg(feature = "async")]
	pub async fn closed(&self) -> Error {
		closed(self.issue.as_ref().expect("unanswered request")).await
	}

	/// Grant the token. The grant holds until the returned [`Issued`] is revoked
	/// or dropped, or the peer withdraws the token.
	///
	/// The session only tells the peer: the origin handles this side serves and
	/// accepts with are what enforce the grant, and revoking it once `expires`
	/// passes is the acceptor's job.
	pub fn accept(mut self, grant: Grant) -> Issued {
		let issue = self.issue.take().expect("answered once");
		issue.lock().outbox.push_back(Reply::Grant(grant));
		Issued { issue }
	}

	/// Refuse the token with a session error code and a reason for the peer.
	pub fn reject(mut self, code: SessionError, reason: impl Into<String>) {
		let issue = self.issue.take().expect("answered once");
		refuse(&issue, code, reason.into());
	}
}

impl Drop for Request {
	fn drop(&mut self) {
		if let Some(issue) = self.issue.take() {
			refuse(&issue, SessionError::Unauthorized, "unanswered".to_string());
		}
	}
}

fn refuse(issue: &kio::Shared<Issue>, code: SessionError, reason: String) {
	let mut issue = issue.lock();
	if !issue.done {
		issue.outbox.push_back(Reply::Refuse { code, reason });
		issue.done = true;
	}
}

/// Which request a token rode on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum RequestKind {
	/// A SUBSCRIBE: the peer reads the named track.
	Subscribe,
	/// A PUBLISH_NAMESPACE: the peer announces a namespace it will publish under.
	PublishNamespace,
}

impl RequestKind {
	/// Whether `grant`, issued to the peer that presented the token, covers this request at
	/// `path`. A grant names what its holder may do, so a request to read is covered by its
	/// `subscribe` patterns and a request to announce by its `publish` patterns.
	#[cfg_attr(not(feature = "async"), allow(dead_code))]
	pub(crate) fn covers(self, grant: &Grant, path: &str) -> bool {
		match self {
			Self::Subscribe => grant.subscribe.matches(path),
			Self::PublishNamespace => grant.publish.matches(path),
		}
	}
}

/// A token on one SUBSCRIBE or PUBLISH_NAMESPACE, waiting for an answer. Experimental.
///
/// The session asks only when its own authorization of the peer does not cover the request,
/// and the application verifies the token: carrying one proves nothing. The grant must cover
/// the request, never joins the session's grant, and ends with the request.
///
/// Dropping it unanswered refuses the request with [`SessionError::Unauthorized`].
pub struct Scoped {
	token: crate::setup::Token,
	path: crate::PathOwned,
	kind: RequestKind,
	issue: Option<kio::Shared<Issue>>,
}

impl Scoped {
	/// The token the request carried.
	pub fn token(&self) -> &crate::setup::Token {
		&self.token
	}

	/// The request's path: the track's namespace, or the namespace announced.
	pub fn path(&self) -> &crate::Path<'static> {
		&self.path
	}

	/// Which request carried the token.
	pub fn kind(&self) -> RequestKind {
		self.kind
	}

	/// Wait for the request to end before an answer, or the session to close.
	#[cfg(feature = "async")]
	pub async fn closed(&self) -> Error {
		closed(self.issue.as_ref().expect("unanswered request")).await
	}

	/// Admit the request on `grant`, which must cover it. The session enforces the grant on
	/// that request alone until the returned [`Enforced`] is revoked or dropped, `expires`
	/// passes, or the request ends.
	pub fn accept(mut self, grant: Grant) -> Enforced {
		let issue = self.issue.take().expect("answered once");
		let mut locked = issue.lock();
		locked.outbox.push_back(Reply::Grant(grant));
		locked.hold = None;
		drop(locked);
		Enforced { issue }
	}

	/// Refuse the request with a session error code and a reason for the peer.
	pub fn reject(mut self, code: SessionError, reason: impl Into<String>) {
		let issue = self.issue.take().expect("answered once");
		refuse(&issue, code, reason.into());
		issue.lock().hold = None;
	}
}

impl Drop for Scoped {
	fn drop(&mut self) {
		if let Some(issue) = self.issue.take() {
			refuse(&issue, SessionError::Unauthorized, "unanswered".to_string());
			issue.lock().hold = None;
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
		handle.verify_request(token, held.path.clone(), held.kind, held.hold.clone())
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
	/// ([`Error::Unauthorized`]), the acceptor revoking or dropping the grant (its code and
	/// reason), or an acceptor-side update that no longer covers the request
	/// ([`Error::Unauthorized`]). A covering update, such as a lowered expiry, is folded in and
	/// polling continues. Never resolves while the grant still stands.
	pub(crate) fn poll_ended(&self, waiter: &kio::Waiter) -> Poll<Refusal> {
		let mut held = self.held();
		loop {
			if held.deadline.poll(waiter).is_ready() {
				return Poll::Ready(Refusal::new(Error::Unauthorized, "request token expired"));
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
	pub(crate) async fn guard<F: std::future::Future>(
		grant: Option<&Self>,
		body: F,
	) -> std::result::Result<F::Output, Refusal> {
		until(body, |waiter| match grant {
			Some(grant) => grant.poll_ended(waiter),
			None => Poll::Pending,
		})
		.await
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

/// A grant issued to one of the peer's session tokens, which the peer is told. Dropping it
/// ends the grant.
pub struct Issued {
	issue: kio::Shared<Issue>,
}

impl Issued {
	/// Replace the grant, such as with a lowered expiry.
	pub fn update(&self, grant: Grant) {
		update(&self.issue, grant);
	}

	/// Revoke the grant with a session error code and a reason for the peer.
	pub fn revoke(self, code: SessionError, reason: impl Into<String>) {
		refuse(&self.issue, code, reason.into());
	}

	/// Wait for the peer to withdraw the token ([`Error::Cancel`]), or for its
	/// stream or the session to end.
	#[cfg(feature = "async")]
	pub async fn closed(&self) -> Error {
		closed(&self.issue).await
	}
}

impl Drop for Issued {
	fn drop(&mut self) {
		self.issue.lock().done = true;
	}
}

/// A grant one request holds on the token it carried, enforced by this side and never told
/// to the peer. Dropping it ends the request.
pub struct Enforced {
	issue: kio::Shared<Issue>,
}

impl Enforced {
	/// Replace the grant, such as with a lowered expiry. One that no longer covers the
	/// request ends it.
	pub fn update(&self, grant: Grant) {
		update(&self.issue, grant);
	}

	/// End the request with a session error code and a reason.
	pub fn revoke(self, code: SessionError, reason: impl Into<String>) {
		refuse(&self.issue, code, reason.into());
	}

	/// Wait for the request to end ([`Error::Cancel`]), or for the session to close.
	#[cfg(feature = "async")]
	pub async fn closed(&self) -> Error {
		closed(&self.issue).await
	}
}

impl Drop for Enforced {
	fn drop(&mut self) {
		self.issue.lock().done = true;
	}
}

fn update(issue: &kio::Shared<Issue>, grant: Grant) {
	let mut issue = issue.lock();
	if !issue.done {
		issue.outbox.push_back(Reply::Grant(grant));
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
}

/// Which part of a [`Permit`] a [`Gate`] watches.
#[derive(Clone, Copy)]
#[cfg_attr(not(feature = "async"), allow(dead_code))]
enum Part {
	/// The peer's grant and the limit both.
	Both,
	/// The peer's grant alone: a token the peer presented stands in for the limit.
	Granted,
	/// The limit alone: a token we presented stands in for the peer's grant.
	Limit,
}

impl Gate {
	#[cfg_attr(not(feature = "async"), allow(dead_code))]
	pub(crate) fn new(handle: Handle, path: crate::PathOwned, direction: Direction) -> Self {
		Self {
			handle,
			path,
			direction,
			epoch: 0,
			part: Part::Both,
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

	/// A gate on the limit alone, for our request our own token authorized: the peer
	/// verifies the token, but a narrowed limit still ends the request.
	#[cfg_attr(not(feature = "async"), allow(dead_code))]
	pub(crate) fn limit(handle: Handle, path: crate::PathOwned, direction: Direction) -> Self {
		Self {
			part: Part::Limit,
			..Self::new(handle, path, direction)
		}
	}

	/// Resolve once the session no longer allows the path. A union that is still
	/// `None` (no answer yet, or a version without AUTH) and a session never limited
	/// allow everything.
	#[cfg_attr(not(feature = "async"), allow(dead_code))]
	pub(crate) fn poll_denied(&mut self, waiter: &kio::Waiter) -> Poll<()> {
		loop {
			let permit = ready_or!(self.handle.poll_permit(self.direction, &mut self.epoch, waiter));
			let path = self.path.as_str();
			let allowed = match self.part {
				Part::Both => permit.matches(path),
				Part::Granted => permit.granted(path),
				Part::Limit => permit.within_limit(path),
			};
			if !allowed {
				return Poll::Ready(());
			}
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

	/// Admit one broadcast [`Self::poll`] found uncovered, such as one a request token carries.
	#[cfg_attr(not(feature = "async"), allow(dead_code))]
	pub(crate) fn admit(&mut self, path: crate::PathOwned) {
		self.live.insert(path);
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn pending_tokens_are_bounded() {
		let handle = Handle::new(true);
		let _requests = handle.requests().unwrap();
		let queue = handle.acceptor().unwrap();
		for _ in 0..128 {
			queue.push(Request::new(Bytes::new(), Default::default()));
		}
		let overflow = kio::Shared::<Issue>::default();
		queue.push(Request::new(Bytes::new(), overflow.clone()));
		let overflow = overflow.lock();
		assert!(overflow.done);
		assert!(matches!(
			overflow.outbox.front(),
			Some(Reply::Refuse {
				code: SessionError::TooManyRequests,
				..
			})
		));
	}

	/// Request tokens queue apart from session tokens, so a backlog of one never refuses the
	/// other.
	#[test]
	fn request_tokens_are_bounded_apart_from_session_tokens() {
		let handle = Handle::new(true);
		let _requests = handle.requests().unwrap();
		let mut scoped = handle.scoped().unwrap();
		let queue = handle.acceptor().unwrap();
		for _ in 0..128 {
			queue.push(Request::new(Bytes::new(), Default::default()));
		}
		let token = crate::setup::Token {
			kind: 0,
			value: b"jwt".to_vec(),
		};
		let path = crate::Path::new("room").to_owned();
		let _verdict = handle.verify_request(token, path, RequestKind::Subscribe, None);
		assert!(
			matches!(scoped.poll_next(&kio::Waiter::noop()), Poll::Ready(Some(_))),
			"a full session queue refused a request token"
		);
	}

	/// Only an application that takes [`Handle::scoped`] verifies request tokens, and only once.
	#[test]
	fn request_tokens_are_opt_in() {
		let handle = Handle::new(true);
		let _requests = handle.requests().unwrap();
		let path = || crate::Path::new("room").to_owned();
		let token = || crate::setup::Token {
			kind: 0,
			value: b"jwt".to_vec(),
		};
		assert!(
			!handle.token_decides(Direction::Publish, "room"),
			"a session never limited and with no verifier ignores the token"
		);
		let verdict = handle.verify_request(token(), path(), RequestKind::Subscribe, None);
		assert!(matches!(
			verdict.poll_grant(&kio::Waiter::noop()),
			Poll::Ready(Err(Refusal {
				error: Error::Unsupported,
				..
			}))
		));

		let _scoped = handle.scoped().unwrap();
		assert!(matches!(handle.scoped(), Err(Error::Duplicate)));
		assert!(handle.token_decides(Direction::Publish, "room"));

		let lite = Handle::new(false);
		lite.lite();
		assert!(matches!(lite.scoped(), Err(Error::Unsupported)));
		assert!(matches!(lite.set_request_token(Some(token())), Err(Error::Unsupported)));
		lite.set_request_token(None).expect("clearing is always allowed");
	}

	#[test]
	fn withdrawn_tokens_do_not_occupy_pending_slots() {
		let handle = Handle::new(true);
		let mut requests = handle.requests().unwrap();
		let queue = handle.acceptor().unwrap();
		let expired = kio::Shared::<Issue>::default();
		for _ in 0..128 {
			queue.push(Request::new(Bytes::from_static(b"withdrawn"), expired.clone()));
		}
		expired.lock().peer = Some(Error::Cancel);
		queue.push(Request::new(Bytes::from_static(b"live"), Default::default()));
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

	/// A request token reaches the app's acceptor tagged with the request it rode on, and
	/// the grant the app answers is what the verdict resolves to.
	#[moq_net_sim::test]
	async fn a_request_token_reaches_the_acceptor_with_its_context() {
		let handle = Handle::new(true);
		let mut requests = handle.scoped().expect("take the requests");
		let verdict = handle.verify_request(token(b"jwt", 7), subscribe_path(), RequestKind::Subscribe, None);

		let request = requests.next().await.expect("a request");
		assert_eq!(request.token(), &token(b"jwt", 7));
		assert_eq!(request.path(), &subscribe_path());
		assert_eq!(request.kind(), RequestKind::Subscribe);
		let _issued = request.accept(Grant::all());

		let grant = verdict.grant().await.expect("granted");
		assert!(grant.publish.matches("room/alice"));
	}

	/// A request-borne token is verified even without the MoQ Auth extension: it rides the
	/// request message, not the AUTH stream, so `scoped()` hands a live queue on a session
	/// whose `supported` is false and the acceptor admits it. This is the shape a standard
	/// moq-transport peer (an encoder or CDN) presents when it does not negotiate the moq-dev
	/// AUTH extension.
	#[moq_net_sim::test]
	async fn a_request_token_is_verified_without_the_auth_extension() {
		let handle = Handle::new(false);
		let mut requests = handle.scoped().expect("take the requests");
		for kind in [RequestKind::Subscribe, RequestKind::PublishNamespace] {
			let verdict = handle.verify_request(token(b"jwt", 0), subscribe_path(), kind, None);
			let request = requests.next().await.expect("a request reaches the acceptor");
			assert_eq!(request.kind(), kind);
			let _issued = request.accept(Grant::all());
			assert!(
				verdict.grant().await.expect("granted").publish.matches("room/alice"),
				"{kind:?} admitted without the AUTH extension"
			);
		}
	}

	/// With no `scoped()` consumer, a request token cannot be verified in band, so the verdict
	/// is Unsupported and the caller refuses the request NOT_SUPPORTED.
	#[moq_net_sim::test]
	async fn no_consumer_refuses_a_request_token_as_unsupported() {
		let handle = Handle::new(true);
		let verdict = handle.verify_request(token(b"jwt", 0), subscribe_path(), RequestKind::Subscribe, None);
		let err = verdict.grant().await.expect_err("no verifier");
		assert!(matches!(err.error, Error::Unsupported), "{err:?}");
	}

	/// A refused request token resolves to the refusal, which the caller sends the peer as
	/// UNAUTHORIZED.
	#[moq_net_sim::test]
	async fn a_refused_request_token_is_unauthorized() {
		let handle = Handle::new(true);
		let mut requests = handle.scoped().unwrap();
		let verdict = handle.verify_request(token(b"jwt", 0), subscribe_path(), RequestKind::Subscribe, None);
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
		let mut requests = handle.scoped().unwrap();

		let verdict = handle.verify_request(token(b"jwt", 0), subscribe_path(), RequestKind::Subscribe, None);
		let issued = requests.next().await.unwrap().accept(Grant::all());
		issued.revoke(SessionError::Unauthorized, "revoked");
		let err = verdict.grant().await.expect_err("revoked before admission");
		assert_eq!(err.reason, "revoked");

		let verdict = handle.verify_request(token(b"jwt", 1), subscribe_path(), RequestKind::Subscribe, None);
		drop(requests.next().await.unwrap().accept(Grant::all()));
		let err = verdict.grant().await.expect_err("dropped before admission");
		assert!(
			matches!(err.error, Error::Session(SessionError::Unauthorized)),
			"{err:?}"
		);

		let verdict = handle.verify_request(token(b"jwt", 2), subscribe_path(), RequestKind::Subscribe, None);
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
		let mut requests = handle.scoped().unwrap();
		let verdict = handle.verify_request(token(b"jwt", 0), subscribe_path(), RequestKind::Subscribe, None);
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
		let mut requests = handle.scoped().unwrap();
		let verdict = handle.verify_request(token(b"jwt", 0), subscribe_path(), RequestKind::Subscribe, None);
		let _issued = requests.next().await.unwrap().accept(Grant::all());
		verdict.grant().await.expect("granted");
		// The union only reflects tokens presented on this side (none here), never a
		// request token the acceptor answered.
		assert_eq!(handle.grant().peek(), None, "a request token must not widen the union");
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
		let mut requests = handle.scoped().unwrap();
		let verdict = handle.verify_request(token(b"jwt", 0), subscribe_path(), RequestKind::Subscribe, None);
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
		let mut requests = handle.scoped().unwrap();
		let expired = || Grant {
			expires: Some(runtime.now()),
			..grant_in(&runtime, 0)
		};

		let verdict = handle.verify_request(token(b"old", 0), subscribe_path(), RequestKind::Subscribe, None);
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

		let verdict = handle.verify_request(token(b"jwt", 0), subscribe_path(), RequestKind::Subscribe, None);
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
		let renewal = handle.verify_request(token(b"old2", 0), subscribe_path(), RequestKind::Subscribe, None);
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
		let mut requests = handle.scoped().unwrap();

		let verdict = handle.verify_request(token(b"jwt", 0), subscribe_path(), RequestKind::Subscribe, None);
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
		let renewal = handle.verify_request(token(b"jwt2", 0), subscribe_path(), RequestKind::Subscribe, None);
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

	/// With no accepted renewal, the deadline ends the request UNAUTHORIZED, and the
	/// session is not closed by it.
	#[moq_net_sim::test]
	async fn the_deadline_ends_the_request_unauthorized_without_renewal() {
		let runtime = crate::time::Clock::sim();
		let handle = Handle::new(true);
		let mut requests = handle.scoped().unwrap();
		let verdict = handle.verify_request(token(b"jwt", 0), subscribe_path(), RequestKind::Subscribe, None);
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
		assert!(matches!(err, Error::Unauthorized), "{err:?}");
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
		let mut requests = handle.scoped().unwrap();
		let verdict = handle.verify_request(token(b"jwt", 0), subscribe_path(), RequestKind::Subscribe, None);
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
		let renewal = handle.verify_request(token(b"bad", 0), subscribe_path(), RequestKind::Subscribe, None);
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
		assert!(matches!(err, Error::Unauthorized), "{err:?}");
	}

	/// An acceptor-side update that no longer covers the request ends it, even when the
	/// replacement has no expiry to lapse at; one that still covers it is folded in.
	#[moq_net_sim::test]
	async fn an_update_that_stops_covering_ends_the_request() {
		let runtime = crate::time::Clock::sim();
		let handle = Handle::new(true);
		let mut requests = handle.scoped().unwrap();
		let verdict = handle.verify_request(token(b"jwt", 0), subscribe_path(), RequestKind::Subscribe, None);
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

	/// An acceptor-side revoke ends the request before the deadline, and does not close the
	/// session.
	#[moq_net_sim::test]
	async fn an_acceptor_revoke_ends_the_request() {
		let runtime = crate::time::Clock::sim();
		let handle = Handle::new(true);
		let mut requests = handle.scoped().unwrap();
		let verdict = handle.verify_request(token(b"jwt", 0), subscribe_path(), RequestKind::Subscribe, None);
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
