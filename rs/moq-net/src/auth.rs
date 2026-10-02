//! In-band authorization: present tokens to the peer and learn what they grant.
//!
//! Each side of a session presents the credential its connection
//! already carried (the URL, a client certificate, or nothing) right after
//! setup, and learns the [`Grant`] it earned. [`Session::auth`](crate::Session::auth)
//! returns the [`Handle`]: [`grant`](Handle::grant) is the union of every token
//! this side presented, and [`add`](Handle::add) presents another without
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
//! AUTH exchange: there the grant stays `None` and [`add`](Handle::add) fails with
//! [`Error::Unsupported`]. moq-lite carries a grant's patterns as they are;
//! moq-transport carries namespace prefixes, so it refuses a grant that is not a
//! union of subtrees rather than widen it.

use std::{
	collections::{BTreeMap, VecDeque},
	task::Poll,
};

use bytes::Bytes;

use crate::{Error, Patterns, Result, SessionError, time::Instant};

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
}

/// One presented token.
pub(crate) struct Slot {
	token: Bytes,
	/// Presented by the session itself at setup, so enforcement waits on it.
	setup: bool,
	/// The latest AUTH_OK, `None` before the first and after the token ends.
	grant: Option<Grant>,
	/// The first reply: `Ok` for an AUTH_OK, the refusal otherwise.
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
	App(kio::Queue<Request>),
	/// The session answers from its own origin handles.
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
		self.granted.as_ref().is_none_or(|granted| granted.matches(path)) && self.within_limit(path)
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

/// The `AUTHORIZATION TOKEN` this side presents on its own SUBSCRIBE and PUBLISH_NAMESPACE
/// requests (MoQ request-token), shared between a session's [`Handle`] and its driver.
///
/// The session reads the current value when it first sends a request, and re-presents a
/// changed one on each live request as a REQUEST_UPDATE. The default presents no token,
/// which is byte-identical to a session that never sets one.
#[derive(Clone, Default)]
pub(crate) struct RequestToken {
	token: kio::Shared<Option<bytes::Bytes>>,
}

impl RequestToken {
	/// A credential presenting `token` (or none) until replaced.
	#[cfg(test)]
	pub(crate) fn new(token: Option<bytes::Bytes>) -> Self {
		Self {
			token: kio::Shared::new(token),
		}
	}

	/// Replace the token presented on this session's requests. A live request re-presents
	/// it as a REQUEST_UPDATE on its next turn; setting the same value again is a no-op.
	pub(crate) fn set(&self, token: Option<Bytes>) {
		*self.token.lock() = token;
	}

	/// The token to present right now, read when a request is first sent.
	pub(crate) fn peek(&self) -> Option<bytes::Bytes> {
		self.token.read().clone()
	}

	/// Ready with the current token once it differs from `last`, registering `waiter`
	/// otherwise. The send loops park here to re-present a replaced token on their live
	/// requests; it reads without advancing `last`, so a poll that loses its turn to
	/// another arm is re-offered the change rather than dropping it.
	pub(crate) fn poll_changed(
		&self,
		last: &Option<bytes::Bytes>,
		waiter: &kio::Waiter,
	) -> std::task::Poll<Option<bytes::Bytes>> {
		use std::task::Poll;
		match self.token.poll(waiter, |cur| match **cur == *last {
			true => Poll::Pending,
			false => Poll::Ready(()),
		}) {
			Poll::Ready(guard) => Poll::Ready((*guard).clone()),
			Poll::Pending => Poll::Pending,
		}
	}
}

/// The session's auth handle, returned by [`Session::auth`](crate::Session::auth).
///
/// Cheap to clone; every clone shares the session's tokens.
#[derive(Clone)]
pub struct Handle {
	state: kio::Shared<State>,
	request_token: RequestToken,
}

impl Handle {
	/// A handle for a session that speaks AUTH (`supported`) or never will.
	pub(crate) fn new(supported: bool) -> Self {
		Self {
			state: kio::Shared::new(State {
				supported,
				..Default::default()
			}),
			request_token: RequestToken::default(),
		}
	}

	/// Present `token` as the `AUTHORIZATION TOKEN` on this side's own requests (MoQ
	/// request-token), as distinct from a connection credential, which [`add`](Self::add)
	/// presents for the whole session.
	///
	/// Set it before running the session's driver to present it on the first request.
	/// Replacing it later re-presents the new token on every live request as a
	/// REQUEST_UPDATE, renewing a token-authorized request in place; setting the same value
	/// again sends nothing. Independent of the MoQ Auth extension.
	pub fn set_request_token(&self, token: impl Into<Bytes>) {
		self.request_token.set(Some(token.into()));
	}

	/// The request-token cell the session's publisher and subscriber present from.
	pub(crate) fn request_token(&self) -> RequestToken {
		self.request_token.clone()
	}

	/// Whether this session speaks the AUTH extension. False on a version that cannot
	/// negotiate it, and on a handle whose side does not offer it (`Extensions::auth` off),
	/// so the SETUP omits the option and no
	/// connection credential is presented.
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
	pub async fn add(&self, token: impl Into<Bytes>) -> Result<Token> {
		let token = self.present(token.into(), false)?;
		kio::wait(|waiter| token.poll_answered(waiter)).await?;
		Ok(token)
	}

	/// Queue a token for the driver to present.
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

	/// Answer every token the peer presents, instead of the default acceptor.
	///
	/// Call it before running the session's driver: whoever answers is decided
	/// once, on the driver's first poll, so a later call cannot take over tokens
	/// already in flight. Without it the session grants the peer what its own
	/// origin handles allow for the connection's credential, and refuses any other
	/// token as unsupported.
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
		let queue = kio::Queue::new();
		// A request-borne token rides the request message itself, not the AUTH stream, so the
		// acceptor answers request tokens even on a session that never negotiated the MoQ Auth
		// extension. The queue therefore stays live regardless of `supported`; only session
		// token presentation ([`add`](Self::add)) stays [`Error::Unsupported`] without it.
		state.acceptor = Acceptor::App(queue.clone());
		Ok(Requests { queue })
	}

	/// Verify a token that rode on one request, scoped to that request alone.
	///
	/// The token reaches the same acceptor a session token does, tagged with the
	/// request's path and kind. The grant the acceptor answers covers only this request,
	/// never joins the session union, and ends when the returned [`RequestVerdict`] is
	/// dropped, so it lives exactly as long as the request. With no [`requests`] consumer
	/// the token cannot be verified, so the verdict is [`Error::Unsupported`] and the
	/// caller refuses the request.
	pub(crate) fn verify_request(
		&self,
		token: Bytes,
		token_kind: u64,
		path: crate::PathOwned,
		kind: RequestKind,
	) -> RequestVerdict {
		match self.acceptor() {
			Some(queue) => {
				let issue = kio::Shared::<Issue>::default();
				// A closed queue (the app dropped its Requests) hands the request back, and
				// dropping it refuses the token with Unauthorized.
				let _ = queue.try_push(Request::new_request(token, token_kind, path, kind, issue.clone()));
				RequestVerdict { issue: Some(issue) }
			}
			// No app verifier took the requests: a request token cannot be checked in band.
			None => RequestVerdict { issue: None },
		}
	}

	/// Decide who answers the peer's tokens: the app if it took the requests,
	/// otherwise the session itself (`None`). Idempotent.
	pub(crate) fn acceptor(&self) -> Option<kio::Queue<Request>> {
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
	pub(crate) fn poll_withdrawn(&self, id: u64, waiter: &kio::Waiter) -> Poll<()> {
		let _ = ready_or!(self.state.poll(waiter, |state| match state.tokens.get(&id) {
			Some(slot) if !slot.withdrawn => Poll::Pending,
			_ => Poll::Ready(()),
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
	pub(crate) fn refused(&self, id: u64) {
		let mut state = self.state.lock();
		if state.tokens.contains_key(&id) {
			state.replied = true;
		}
	}

	/// End the token: refused or revoked by the peer, withdrawn by us, or its
	/// stream lost. A token that was never answered records `err` as its answer.
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
	pub(crate) fn poll_union(&self, epoch: &mut u64, waiter: &kio::Waiter) -> Poll<Option<Grant>> {
		let state = ready_or!(self.poll_epoch(*epoch, waiter));
		*epoch = state.epoch;
		Poll::Ready(state.union.clone())
	}

	/// What `direction` allows once it may have changed since `epoch` last saw it,
	/// advancing `epoch`.
	pub(crate) fn poll_permit(&self, direction: Direction, epoch: &mut u64, waiter: &kio::Waiter) -> Poll<Permit> {
		let state = ready_or!(self.poll_epoch(*epoch, waiter));
		*epoch = state.epoch;
		Poll::Ready(state.permit(direction))
	}

	/// The limit once it may have changed since `epoch` last saw it, advancing `epoch`.
	pub(crate) fn poll_limit(&self, epoch: &mut u64, waiter: &kio::Waiter) -> Poll<Option<Grant>> {
		let state = ready_or!(self.poll_epoch(*epoch, waiter));
		*epoch = state.epoch;
		Poll::Ready(state.limit.clone())
	}

	fn poll_epoch(&self, seen: u64, waiter: &kio::Waiter) -> Poll<kio::Mut<'_, State>> {
		self.state.poll(waiter, |state| match state.epoch != seen {
			true => Poll::Ready(()),
			false => Poll::Pending,
		})
	}

	/// Whether `direction` allows `path` right now: the union (allowing everything
	/// while it is `None`, before an answer or on a version without AUTH) and the
	/// limit both.
	pub(crate) fn allows(&self, direction: Direction, path: &str) -> bool {
		self.state.read().allows(direction, path)
	}

	/// Whether the limit alone allows `direction` at `path`: for a route the peer
	/// offers us, which our own grant does not decide.
	pub(crate) fn within_limit(&self, direction: Direction, path: &str) -> bool {
		let state = self.state.read();
		let (_, limit) = state.parts(direction);
		limit.is_none_or(|limit| limit.matches(path))
	}

	/// Whether the union positively covers `path` right now. Unlike [`allows`](Self::allows), a
	/// union that is still `None` (no answer yet, or a version without AUTH) does NOT cover: a
	/// request presenting a token is verified by it rather than admitted by the permissive
	/// default. Only the token-bearing path uses this; the token-less path keeps `allows`.
	pub(crate) fn covers(&self, direction: Direction, path: &str) -> bool {
		let state = self.state.read();
		state.union.as_ref().is_some_and(|union| match direction {
			Direction::Publish => union.publish.matches(path),
			Direction::Subscribe => union.subscribe.matches(path),
		})
	}

	/// The peer turned out not to negotiate AUTH: fail every token as unsupported and
	/// close the requests, leaving the union unknown.
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
		// The request queue stays live: a request-borne token does not ride the AUTH stream, so
		// the acceptor keeps answering request tokens without the extension. Only the session
		// tokens above end as unsupported.
	}

	/// End the session: fail every pending token, end every watch, and close the
	/// requests.
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
	pub async fn changed(&mut self) -> Result<Option<Grant>> {
		kio::wait(|waiter| self.poll_changed(waiter)).await
	}
}

/// The tokens the peer presents, for an application that answers them itself.
///
/// Returned by [`Handle::requests`]. Dropping it refuses every later token.
pub struct Requests {
	queue: kio::Queue<Request>,
}

impl Requests {
	/// Poll for the next token, or `None` once the session ends.
	pub fn poll_next(&mut self, waiter: &kio::Waiter) -> Poll<Option<Request>> {
		self.queue.poll_pop(waiter).map(|res| res.ok())
	}

	/// Wait for the next token the peer presents, or `None` once the session ends.
	pub async fn next(&mut self) -> Option<Request> {
		self.queue.pop().await.ok()
	}
}

impl Drop for Requests {
	fn drop(&mut self) {
		self.queue.close();
		// The session keeps a handle to the queue, so drop what is already queued here:
		// each request refuses its token as it drops.
		while let Ok(Some(_)) = self.queue.try_pop() {}
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
}

/// The serving task's hold on one peer token's [`Issue`].
///
/// Dropping it settles [`Issued::closed`] with the session's error, unless the task
/// already recorded why the stream ended, so no exit path leaves the acceptor waiting.
pub(crate) struct Serving {
	pub issue: kio::Shared<Issue>,
	handle: Handle,
}

impl Serving {
	/// A fresh stream's shared state, settled by the session `handle` if the task drops.
	pub(crate) fn new(handle: Handle) -> Self {
		Self {
			issue: kio::Shared::default(),
			handle,
		}
	}

	/// Record why the stream ended on the peer's side; the first reason wins.
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

/// Which request a token rode on. A token on a request authorizes that one request, so
/// the acceptor and the session scope its grant to the request's path and kind rather
/// than to the whole session.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RequestKind {
	/// A SUBSCRIBE: the subscriber reads the named track.
	Subscribe,
	/// A FETCH: the subscriber reads a past range of the named track.
	Fetch,
	/// A PUBLISH: the peer offers a track to publish.
	Publish,
	/// A PUBLISH_NAMESPACE: the peer announces a namespace it will publish under.
	PublishNamespace,
	/// A SUBSCRIBE_NAMESPACE: the peer asks to be told what is published under a prefix.
	SubscribeNamespace,
	/// A TRACK_STATUS: the peer asks for a track's current status.
	TrackStatus,
}

impl RequestKind {
	/// Whether `grant`, issued to the peer that presented the token, covers this request at
	/// `path`. A grant names what its holder may do, so a request to read is covered by its
	/// `subscribe` patterns and a request to announce or publish by its `publish` patterns.
	pub(crate) fn covers(self, grant: &Grant, path: &str) -> bool {
		match self {
			Self::Subscribe | Self::Fetch | Self::SubscribeNamespace | Self::TrackStatus => {
				grant.subscribe.matches(path)
			}
			Self::Publish | Self::PublishNamespace => grant.publish.matches(path),
		}
	}
}

/// The request a token rode on, kept beside the token so the acceptor can scope its
/// grant to that one request.
struct RequestContext {
	path: crate::PathOwned,
	kind: RequestKind,
	/// The Token structure's type (section 8.9): which verifier the token is for.
	token_kind: u64,
}

/// A token the peer presented, waiting for an answer.
///
/// A token with no [`path`](Request::path) is the connection's own credential (or an
/// AUTH-stream token), granting the whole session. A token that rode on a request carries
/// that request's [`path`](Request::path) and [`kind`](Request::kind): its grant covers
/// only that request and never joins the session union.
///
/// Dropping it unanswered refuses the token with [`SessionError::Unauthorized`].
pub struct Request {
	token: Bytes,
	issue: Option<kio::Shared<Issue>>,
	/// `Some` when the token rode on a request; `None` for the connection credential.
	context: Option<RequestContext>,
}

impl Request {
	pub(crate) fn new(token: Bytes, issue: kio::Shared<Issue>) -> Self {
		Self {
			token,
			issue: Some(issue),
			context: None,
		}
	}

	/// A token that rode on one request, carrying the credential value, its structure
	/// type, and the request it belongs to.
	pub(crate) fn new_request(
		token: Bytes,
		token_kind: u64,
		path: crate::PathOwned,
		kind: RequestKind,
		issue: kio::Shared<Issue>,
	) -> Self {
		Self {
			token,
			issue: Some(issue),
			context: Some(RequestContext { path, kind, token_kind }),
		}
	}

	/// The token the peer presented. Empty means the credential its connection
	/// already carried, or none. For a request token this is the structure's value.
	pub fn token(&self) -> &Bytes {
		&self.token
	}

	/// The request this token rode on, or `None` for the connection's own credential.
	pub fn path(&self) -> Option<&str> {
		self.context.as_ref().map(|c| c.path.as_str())
	}

	/// Which request this token rode on, or `None` for the connection's own credential.
	pub fn kind(&self) -> Option<RequestKind> {
		self.context.as_ref().map(|c| c.kind)
	}

	/// The Token structure's type (section 8.9), for a request token: which verifier it
	/// is for (a CAT reaches the CAT verifier, not the JWT one). `None` for the
	/// connection's own credential.
	pub fn token_kind(&self) -> Option<u64> {
		self.context.as_ref().map(|c| c.token_kind)
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
	issue.outbox.push_back(Reply::Refuse { code, reason });
	issue.done = true;
}

/// The verdict on a request-borne token, from [`Handle::verify_request`].
///
/// Holding it keeps the request's grant alive; dropping it tells the acceptor the request
/// is over, so the grant lives exactly as long as the request and never outlives it.
pub(crate) struct RequestVerdict {
	issue: Option<kio::Shared<Issue>>,
}

impl RequestVerdict {
	/// Wait for the acceptor's first answer: the grant it issued, or a refusal as the
	/// [`Error`] whose request code the caller sends the peer. No consumer is
	/// [`Error::Unsupported`]; an unanswered (dropped) request is
	/// [`SessionError::Unauthorized`].
	pub(crate) async fn grant(&self) -> Result<Grant> {
		kio::wait(|waiter| self.poll_grant(waiter)).await
	}

	/// Poll for the acceptor's first answer, so the caller can race the verify against the
	/// live grant's deadline and serving rather than blocking on a bare await.
	pub(crate) fn poll_grant(&self, waiter: &kio::Waiter) -> Poll<Result<Grant>> {
		let Some(issue) = &self.issue else {
			return Poll::Ready(Err(Error::Unsupported));
		};
		let mut guard = ready_or!(
			issue.poll(waiter, |issue| match issue.outbox.is_empty() && !issue.done {
				true => Poll::Pending,
				false => Poll::Ready(()),
			})
		);
		Poll::Ready(match guard.outbox.pop_front() {
			Some(Reply::Grant(grant)) => Ok(grant),
			Some(Reply::Refuse { code, .. }) => Err(Error::Session(code)),
			// Done with nothing written: the acceptor dropped the request unanswered.
			None => Err(Error::Session(SessionError::Unauthorized)),
		})
	}

	/// After the first grant, poll for the acceptor's next action on this token: a
	/// replacement grant (an update, such as a lowered expiry), a refusal (revoke), or the
	/// issued grant being dropped. `Pending` while the grant still stands.
	pub(crate) fn poll_reply(&self, waiter: &kio::Waiter) -> Poll<Reply> {
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
			// Done with nothing more to read: the acceptor dropped the issued grant, ending
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
			// Tell the acceptor's Issued the request is over, so it stops revalidating this
			// request's grant. An unread grant left in the outbox does not matter: the
			// request is ending regardless.
			issue.lock().peer.get_or_insert(Error::Cancel);
		}
	}
}

/// A request-borne grant, held for the life of the request it authorized (a SUBSCRIBE, a
/// FETCH, ...). It carries the acceptor's grant and a deadline armed at the grant's
/// expiry, and ends the request when the deadline lapses, the acceptor revokes or drops
/// the grant, or a refused renewal leaves the old grant to lapse. A REQUEST_UPDATE the
/// acceptor accepts [`renew`](Self::renew)s it with a fresh grant and expiry.
///
/// Ending a request grant never touches the session: only that one request ends.
pub(crate) struct RequestGrant<R: crate::runtime::Timers> {
	verdict: RequestVerdict,
	grant: Grant,
	deadline: crate::runtime::Deadline<R>,
	/// The request this grant must keep covering: a replacement that no longer does ends it.
	path: crate::PathOwned,
	kind: RequestKind,
}

impl<R: crate::runtime::Timers> RequestGrant<R> {
	/// Hold `grant` (the acceptor's first answer, already awaited) for the request's life,
	/// armed to lapse at its expiry.
	pub(crate) fn new(
		runtime: &R,
		verdict: RequestVerdict,
		grant: Grant,
		path: crate::PathOwned,
		kind: RequestKind,
	) -> Self {
		let mut deadline = crate::runtime::Deadline::new(runtime);
		deadline.set(grant.expires);
		Self {
			verdict,
			grant,
			deadline,
			path,
			kind,
		}
	}

	/// Whether `grant` covers the request this guard holds.
	pub(crate) fn covers(&self, grant: &Grant) -> bool {
		self.kind.covers(grant, self.path.as_str())
	}

	/// The grant in force right now. Observed by the lifecycle tests; the reader checks a
	/// renewal's coverage on the freshly awaited grant before it calls [`renew`](Self::renew).
	#[cfg_attr(not(test), expect(dead_code))]
	pub(crate) fn grant(&self) -> &Grant {
		&self.grant
	}

	/// Replace the grant after an accepted REQUEST_UPDATE: adopt the new verdict (dropping
	/// the old, which ends the old token) and re-arm the deadline at the new expiry. A
	/// refused renewal does NOT call this: the old grant stands until it lapses.
	pub(crate) fn renew(&mut self, verdict: RequestVerdict, grant: Grant) {
		self.verdict = verdict;
		self.deadline.set(grant.expires);
		self.grant = grant;
	}

	/// Resolve with the error that ends the request: the deadline lapsing
	/// ([`Error::Unauthorized`]; [`Error::Expired`] once the code split lands), or the
	/// acceptor revoking or dropping the grant (its code), or an acceptor-side update (a
	/// replacement grant on the same token) that no longer covers the request
	/// ([`Error::Unauthorized`]). A covering update, such as a lowered expiry, is folded in and
	/// polling continues. Never resolves while the grant still stands.
	pub(crate) fn poll_ended(&mut self, waiter: &kio::Waiter) -> Poll<Error> {
		loop {
			if self.deadline.poll(waiter).is_ready() {
				return Poll::Ready(Error::Unauthorized);
			}
			match self.verdict.poll_reply(waiter) {
				Poll::Ready(Reply::Grant(grant)) => {
					if !self.covers(&grant) {
						return Poll::Ready(Error::Unauthorized);
					}
					self.deadline.set(grant.expires);
					self.grant = grant;
					// Re-poll: the new deadline may already have lapsed, or another reply may
					// be waiting.
					continue;
				}
				Poll::Ready(Reply::Refuse { code, .. }) => return Poll::Ready(Error::Session(code)),
				Poll::Pending => return Poll::Pending,
			}
		}
	}
}

/// A grant issued to one of the peer's tokens. Dropping it ends the grant.
pub struct Issued {
	issue: kio::Shared<Issue>,
}

impl Issued {
	/// Replace the grant, such as with a lowered expiry.
	pub fn update(&self, grant: Grant) {
		let mut issue = self.issue.lock();
		if !issue.done {
			issue.outbox.push_back(Reply::Grant(grant));
		}
	}

	/// Revoke the grant with a session error code and a reason for the peer.
	pub fn revoke(self, code: SessionError, reason: impl Into<String>) {
		refuse(&self.issue, code, reason.into());
	}

	/// Wait for the peer to withdraw the token ([`Error::Cancel`]), or for its
	/// stream or the session to end.
	pub async fn closed(&self) -> Error {
		kio::wait(|waiter| {
			let issue = ready_or!(self.issue.poll(waiter, |issue| match issue.peer.is_some() {
				true => Poll::Ready(()),
				false => Poll::Pending,
			}));
			Poll::Ready(issue.peer.clone().expect("checked above"))
		})
		.await
	}
}

impl Drop for Issued {
	fn drop(&mut self) {
		self.issue.lock().done = true;
	}
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
	handle: Handle,
	path: crate::PathOwned,
	direction: Direction,
	epoch: u64,
	/// Watch the limit alone, for a request a token authorized in place of the union.
	limit_only: bool,
}

impl Gate {
	pub(crate) fn new(handle: Handle, path: crate::PathOwned, direction: Direction) -> Self {
		Self {
			handle,
			path,
			direction,
			epoch: 0,
			limit_only: false,
		}
	}

	/// A gate on the limit alone: a request token stands in for the union, never for the
	/// ceiling this side set on the peer, so a later narrowing still ends the request.
	pub(crate) fn limit(handle: Handle, path: crate::PathOwned, direction: Direction) -> Self {
		Self {
			limit_only: true,
			..Self::new(handle, path, direction)
		}
	}

	/// Resolve once the session no longer allows the path. A union that is still
	/// `None` (no answer yet, or a version without AUTH) and a session never limited
	/// allow everything.
	pub(crate) fn poll_denied(&mut self, waiter: &kio::Waiter) -> Poll<()> {
		loop {
			let permit = ready_or!(self.handle.poll_permit(self.direction, &mut self.epoch, waiter));
			let allowed = match self.limit_only {
				true => permit.within_limit(self.path.as_str()),
				false => permit.matches(self.path.as_str()),
			};
			if !allowed {
				return Poll::Ready(());
			}
		}
	}
}

/// The default acceptor's answer to the peer's connection credential: the grant
/// the session's origin handles allow, within the limit as it changes.
pub(crate) struct DefaultGrant {
	handle: Handle,
	base: Grant,
	epoch: u64,
	limit: Option<Grant>,
	/// The grant last handed out, so an unrelated wakeup sends nothing.
	sent: Option<Grant>,
}

impl DefaultGrant {
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
	live: std::collections::HashSet<crate::PathOwned>,
	setup: bool,
}

impl Enforce {
	/// Resolve with the first broadcast announced outside the union, or `None` once the
	/// origin ends.
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
			let Some(update) = ready_or!(announced.poll_next(waiter)) else {
				return Poll::Ready(None);
			};
			match update.kind {
				crate::announce::Kind::Announced if !self.live.contains(&update.prefix) => {
					if !permit.matches(update.prefix.as_str()) {
						return Poll::Ready(Some(update.prefix));
					}
					self.live.insert(update.prefix);
				}
				crate::announce::Kind::Retracted => {
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

#[cfg(test)]
mod request_token_tests {
	use super::*;

	fn subscribe_path() -> crate::PathOwned {
		crate::Path::new("room/alice").to_owned()
	}

	/// A request token reaches the app's acceptor tagged with the request it rode on, and
	/// the grant the app answers is what the verdict resolves to.
	#[tokio::test]
	async fn a_request_token_reaches_the_acceptor_with_its_context() {
		let handle = Handle::new(true);
		let mut requests = handle.requests().expect("take the requests");
		let verdict = handle.verify_request(Bytes::from_static(b"jwt"), 7, subscribe_path(), RequestKind::Subscribe);

		let request = requests.next().await.expect("a request");
		assert_eq!(request.token(), &Bytes::from_static(b"jwt"));
		assert_eq!(request.path(), Some("room/alice"));
		assert_eq!(request.kind(), Some(RequestKind::Subscribe));
		assert_eq!(request.token_kind(), Some(7));
		let _issued = request.accept(Grant::all());

		let grant = verdict.grant().await.expect("granted");
		assert!(grant.publish.matches("room/alice"));
	}

	/// A request-borne token is verified even without the MoQ Auth extension: it rides the
	/// request message, not the AUTH stream, so `requests()` hands a live queue on a session
	/// whose `supported` is false and the acceptor admits it. This is the shape a standard
	/// moq-transport peer (an encoder or CDN) presents when it does not negotiate the moq-dev
	/// AUTH extension.
	#[tokio::test]
	async fn a_request_token_is_verified_without_the_auth_extension() {
		let handle = Handle::new(false);
		let mut requests = handle.requests().expect("take the requests");
		for kind in [RequestKind::Subscribe, RequestKind::PublishNamespace] {
			let verdict = handle.verify_request(Bytes::from_static(b"jwt"), 0, subscribe_path(), kind);
			let request = requests.next().await.expect("a request reaches the acceptor");
			assert_eq!(request.kind(), Some(kind));
			let _issued = request.accept(Grant::all());
			assert!(
				verdict.grant().await.expect("granted").publish.matches("room/alice"),
				"{kind:?} admitted without the AUTH extension"
			);
		}
	}

	/// With no `requests()` consumer, a request token cannot be verified in band, so the	/// verdict is Unsupported and the caller refuses the request NOT_SUPPORTED.
	#[tokio::test]
	async fn no_consumer_refuses_a_request_token_as_unsupported() {
		let handle = Handle::new(true);
		let verdict = handle.verify_request(Bytes::from_static(b"jwt"), 0, subscribe_path(), RequestKind::Subscribe);
		let err = verdict.grant().await.expect_err("no verifier");
		assert!(matches!(err, Error::Unsupported), "{err:?}");
	}

	/// A refused request token resolves to the refusal, which the caller sends the peer as
	/// UNAUTHORIZED.
	#[tokio::test]
	async fn a_refused_request_token_is_unauthorized() {
		let handle = Handle::new(true);
		let mut requests = handle.requests().unwrap();
		let verdict = handle.verify_request(Bytes::from_static(b"jwt"), 0, subscribe_path(), RequestKind::Subscribe);
		requests.next().await.unwrap().reject(SessionError::Unauthorized, "no");
		let err = verdict.grant().await.expect_err("refused");
		assert!(matches!(err, Error::Session(SessionError::Unauthorized)), "{err:?}");
	}

	/// The grant lives exactly as long as the request: dropping the verdict ends the
	/// acceptor's issued grant, so it never outlives the request nor joins the union.
	#[tokio::test]
	async fn dropping_the_verdict_ends_the_grant() {
		let handle = Handle::new(true);
		let mut requests = handle.requests().unwrap();
		let verdict = handle.verify_request(Bytes::from_static(b"jwt"), 0, subscribe_path(), RequestKind::Subscribe);
		let issued = requests.next().await.unwrap().accept(Grant::all());
		verdict.grant().await.expect("granted");
		drop(verdict);
		let err = issued.closed().await;
		assert!(matches!(err, Error::Cancel), "{err:?}");
	}

	/// A request token never joins the session union: the union stays what the connection
	/// credential earned, not what a request token granted.
	#[tokio::test]
	async fn a_request_token_never_joins_the_union() {
		let handle = Handle::new(true);
		let mut requests = handle.requests().unwrap();
		let verdict = handle.verify_request(Bytes::from_static(b"jwt"), 0, subscribe_path(), RequestKind::Subscribe);
		let _issued = requests.next().await.unwrap().accept(Grant::all());
		verdict.grant().await.expect("granted");
		// The union only reflects tokens presented on this side (none here), never a
		// request token the acceptor answered.
		assert_eq!(handle.grant().peek(), None, "a request token must not widen the union");
	}

	/// A session token (the connection credential) carries no request context, so the
	/// acceptor can tell it apart from a request token.
	#[test]
	fn a_session_token_has_no_request_context() {
		let request = Request::new(Bytes::new(), kio::Shared::<Issue>::default());
		assert_eq!(request.path(), None);
		assert_eq!(request.kind(), None);
		assert_eq!(request.token_kind(), None);
	}

	use std::time::Duration;

	fn grant_in(runtime: &crate::time::Clock, secs: u64) -> Grant {
		Grant {
			publish: Patterns::new(),
			subscribe: crate::Pattern::all().into(),
			expires: crate::runtime::Timers::now(runtime).checked_add(Duration::from_secs(secs)),
		}
	}

	/// The request grant carries the acceptor's expiry, which is the deadline it lapses at.
	#[tokio::test(start_paused = true)]
	async fn a_request_grant_has_the_acceptors_expiry() {
		let runtime = crate::time::Clock::tokio();
		let handle = Handle::new(true);
		let mut requests = handle.requests().unwrap();
		let verdict = handle.verify_request(Bytes::from_static(b"jwt"), 0, subscribe_path(), RequestKind::Subscribe);
		let grant = grant_in(&runtime, 60);
		let expires = grant.expires;
		let _issued = requests.next().await.unwrap().accept(grant);
		let answered = verdict.grant().await.unwrap();
		let request_grant = RequestGrant::new(&runtime, verdict, answered, subscribe_path(), RequestKind::Subscribe);
		assert_eq!(request_grant.grant().expires, expires);
		assert!(expires.is_some());
	}

	/// An accepted REQUEST_UPDATE replaces the grant and its expiry.
	#[tokio::test(start_paused = true)]
	async fn a_renewal_token_replaces_the_grant_and_expiry() {
		let runtime = crate::time::Clock::tokio();
		let handle = Handle::new(true);
		let mut requests = handle.requests().unwrap();

		let verdict = handle.verify_request(Bytes::from_static(b"jwt"), 0, subscribe_path(), RequestKind::Subscribe);
		let first = grant_in(&runtime, 60);
		let first_expires = first.expires;
		let _issued = requests.next().await.unwrap().accept(first);
		let answered = verdict.grant().await.unwrap();
		let mut request_grant =
			RequestGrant::new(&runtime, verdict, answered, subscribe_path(), RequestKind::Subscribe);

		// A REQUEST_UPDATE carrying a fresh token the acceptor accepts with a later expiry.
		let renewal = handle.verify_request(Bytes::from_static(b"jwt2"), 0, subscribe_path(), RequestKind::Subscribe);
		let second = grant_in(&runtime, 600);
		let second_expires = second.expires;
		let _issued2 = requests.next().await.unwrap().accept(second);
		let renewed = renewal.grant().await.unwrap();
		request_grant.renew(renewal, renewed);

		assert_eq!(request_grant.grant().expires, second_expires);
		assert_ne!(second_expires, first_expires);
	}

	/// With no accepted renewal, the deadline ends the request UNAUTHORIZED, and the
	/// session is not closed by it.
	#[tokio::test(start_paused = true)]
	async fn the_deadline_ends_the_request_unauthorized_without_renewal() {
		let runtime = crate::time::Clock::tokio();
		let handle = Handle::new(true);
		let mut requests = handle.requests().unwrap();
		let verdict = handle.verify_request(Bytes::from_static(b"jwt"), 0, subscribe_path(), RequestKind::Subscribe);
		// Held for the request's life, so only the deadline (not a drop) ends it.
		let _issued = requests.next().await.unwrap().accept(grant_in(&runtime, 60));
		let answered = verdict.grant().await.unwrap();
		let mut request_grant =
			RequestGrant::new(&runtime, verdict, answered, subscribe_path(), RequestKind::Subscribe);

		let err = kio::wait(|waiter| request_grant.poll_ended(waiter)).await;
		assert!(matches!(err, Error::Unauthorized), "{err:?}");
		assert_eq!(handle.grant().peek(), None, "the session grant is untouched");
	}

	/// A refused renewal does not touch the grant: the old grant stands and the request
	/// ends only when that old grant lapses.
	#[tokio::test(start_paused = true)]
	async fn a_refused_renewal_keeps_the_old_grant_until_it_lapses() {
		let runtime = crate::time::Clock::tokio();
		let handle = Handle::new(true);
		let mut requests = handle.requests().unwrap();
		let verdict = handle.verify_request(Bytes::from_static(b"jwt"), 0, subscribe_path(), RequestKind::Subscribe);
		let first = grant_in(&runtime, 60);
		let first_expires = first.expires;
		let _issued = requests.next().await.unwrap().accept(first);
		let answered = verdict.grant().await.unwrap();
		let mut request_grant =
			RequestGrant::new(&runtime, verdict, answered, subscribe_path(), RequestKind::Subscribe);

		// The renewal token is refused: verify it resolves to a refusal, and the caller does
		// NOT renew. The old grant is untouched.
		let renewal = handle.verify_request(Bytes::from_static(b"bad"), 0, subscribe_path(), RequestKind::Subscribe);
		requests
			.next()
			.await
			.unwrap()
			.reject(SessionError::Unauthorized, "bad token");
		assert!(matches!(
			renewal.grant().await,
			Err(Error::Session(SessionError::Unauthorized))
		));

		// The old grant still stands with its original expiry, and the request ends only
		// when that lapses.
		assert_eq!(request_grant.grant().expires, first_expires);
		let err = kio::wait(|waiter| request_grant.poll_ended(waiter)).await;
		assert!(matches!(err, Error::Unauthorized), "{err:?}");
	}

	/// An acceptor-side update that no longer covers the request ends it, even when the
	/// replacement has no expiry to lapse at; one that still covers it is folded in.
	#[tokio::test(start_paused = true)]
	async fn an_update_that_stops_covering_ends_the_request() {
		let runtime = crate::time::Clock::tokio();
		let handle = Handle::new(true);
		let mut requests = handle.requests().unwrap();
		let verdict = handle.verify_request(Bytes::from_static(b"jwt"), 0, subscribe_path(), RequestKind::Subscribe);
		let issued = requests.next().await.unwrap().accept(Grant::all());
		let answered = verdict.grant().await.unwrap();
		let mut request_grant =
			RequestGrant::new(&runtime, verdict, answered, subscribe_path(), RequestKind::Subscribe);

		// A covering update (everything, still unexpiring) leaves the request standing.
		issued.update(Grant::all());
		let pending = kio::wait(|waiter| Poll::Ready(request_grant.poll_ended(waiter).is_pending())).await;
		assert!(pending, "a covering update keeps the request");

		issued.update(Grant::default());
		let err = tokio::time::timeout(
			Duration::from_secs(1),
			kio::wait(|waiter| request_grant.poll_ended(waiter)),
		)
		.await
		.expect("an uncovering update must end the request");
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
	#[tokio::test(start_paused = true)]
	async fn an_acceptor_revoke_ends_the_request() {
		let runtime = crate::time::Clock::tokio();
		let handle = Handle::new(true);
		let mut requests = handle.requests().unwrap();
		let verdict = handle.verify_request(Bytes::from_static(b"jwt"), 0, subscribe_path(), RequestKind::Subscribe);
		// A grant that never expires, so only the revoke can end the request.
		let issued = requests.next().await.unwrap().accept(Grant::all());
		let answered = verdict.grant().await.unwrap();
		let mut request_grant =
			RequestGrant::new(&runtime, verdict, answered, subscribe_path(), RequestKind::Subscribe);

		issued.revoke(SessionError::Unauthorized, "revoked");
		let err = kio::wait(|waiter| request_grant.poll_ended(waiter)).await;
		assert!(matches!(err, Error::Session(SessionError::Unauthorized)), "{err:?}");
		assert_eq!(handle.grant().peek(), None, "the session grant is untouched");
	}
}
