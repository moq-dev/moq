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
//! driver, and then answers every token the peer presents.
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

	fn patterns(&self, direction: Direction) -> &Patterns {
		match direction {
			Direction::Publish => &self.publish,
			Direction::Subscribe => &self.subscribe,
		}
	}

	/// Fold `other` into this grant: the paths either allows, lapsing at the
	/// earlier expiry, which is when the union next shrinks.
	fn union(&mut self, other: &Self) {
		self.publish.extend(other.publish.iter().cloned());
		self.subscribe.extend(other.subscribe.iter().cloned());
		self.expires = match (self.expires, other.expires) {
			(Some(a), Some(b)) => Some(a.min(b)),
			(a, b) => a.or(b),
		};
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
	/// Bumped whenever the union changes, so the session's per-stream gates can
	/// skip re-matching paths on every wakeup.
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
}

/// The session's auth handle, returned by [`Session::auth`](crate::Session::auth).
///
/// Cheap to clone; every clone shares the session's tokens.
#[derive(Clone)]
pub struct Handle {
	state: kio::Shared<State>,
}

impl Handle {
	/// A handle for a session that speaks AUTH (`supported`) or never will.
	pub(crate) fn new(supported: bool) -> Self {
		Self {
			state: kio::Shared::new(State {
				supported,
				..Default::default()
			}),
		}
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
		// A version without AUTH never receives a token, so its requests end with
		// the session like any other.
		if !state.supported {
			queue.close();
		}
		state.acceptor = Acceptor::App(queue.clone());
		Ok(Requests { queue })
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

	/// The union once it changed since `epoch` last saw it, advancing `epoch`. It
	/// only ever changes into `Some`: no answer yet is the initial state.
	pub(crate) fn poll_union(&self, epoch: &mut u64, waiter: &kio::Waiter) -> Poll<Option<Grant>> {
		let seen = *epoch;
		let state = ready_or!(self.state.poll(waiter, |state| match state.epoch != seen {
			true => Poll::Ready(()),
			false => Poll::Pending,
		}));
		*epoch = state.epoch;
		Poll::Ready(state.union.clone())
	}

	/// Whether the union allows `path` right now. A union that is still `None` (no
	/// answer yet, or a version without AUTH) allows everything.
	pub(crate) fn allows(&self, direction: Direction, path: &str) -> bool {
		let state = self.state.read();
		state
			.union
			.as_ref()
			.is_none_or(|union| union.patterns(direction).matches(path))
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
		if let Acceptor::App(queue) = &state.acceptor {
			queue.close();
		}
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

impl Issue {
	/// A fresh stream's shared state.
	pub(crate) fn shared() -> kio::Shared<Self> {
		kio::Shared::default()
	}
}

/// A token the peer presented, waiting for an answer.
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

	/// The token the peer presented. Empty means the credential its connection
	/// already carried, or none.
	pub fn token(&self) -> &Bytes {
		&self.token
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

/// Which half of a grant a [`Gate`] checks.
#[derive(Clone, Copy)]
pub(crate) enum Direction {
	Publish,
	Subscribe,
}

/// Watches whether the union still allows one path, for a long-lived stream that
/// must end once it does not.
///
/// Cheap to poll on every wakeup: the path is only re-matched when the union
/// changes.
pub(crate) struct Gate {
	handle: Handle,
	path: crate::PathOwned,
	direction: Direction,
	epoch: u64,
}

impl Gate {
	pub(crate) fn new(handle: Handle, path: crate::PathOwned, direction: Direction) -> Self {
		Self {
			handle,
			path,
			direction,
			epoch: 0,
		}
	}

	/// Resolve once the union no longer allows the path. A union that is still
	/// `None` (no answer yet, or a version without AUTH) allows everything.
	pub(crate) fn poll_denied(&mut self, waiter: &kio::Waiter) -> Poll<()> {
		loop {
			let Some(union) = ready_or!(self.handle.poll_union(&mut self.epoch, waiter)) else {
				continue;
			};
			if !union.patterns(self.direction).matches(self.path.as_str()) {
				return Poll::Ready(());
			}
		}
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
