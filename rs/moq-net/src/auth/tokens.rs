//! The tokens a reconnecting connection presents on each session, and the peer's answer.

use std::task::Poll;

use bytes::Bytes;
use url::Url;

use super::{Answer, Handle, State};
use crate::{Error, Result};

/// A credential to present when dialing, such as a JWT.
///
/// Its `Debug` shows only the length, so a dumped config never leaks it.
#[derive(Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Token(String);

impl Token {
	/// The credential itself.
	pub fn as_str(&self) -> &str {
		&self.0
	}
}

impl std::fmt::Debug for Token {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		write!(f, "Token(<{} bytes>)", self.0.len())
	}
}

impl std::str::FromStr for Token {
	type Err = EmptyToken;

	fn from_str(token: &str) -> std::result::Result<Self, Self::Err> {
		Self::try_from(token.to_string())
	}
}

impl TryFrom<String> for Token {
	type Error = EmptyToken;

	fn try_from(token: String) -> std::result::Result<Self, Self::Error> {
		match token.is_empty() {
			true => Err(EmptyToken),
			false => Ok(Self(token)),
		}
	}
}

impl From<Token> for String {
	fn from(token: Token) -> Self {
		token.0
	}
}

/// An empty token, which presents no credential.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error("a token cannot be empty")]
#[non_exhaustive]
pub struct EmptyToken;

/// The tokens a connection presents on every session it dials, and the peer's answer to them.
///
/// The first is the connection credential, which each dial carries in SETUP or its URL,
/// unless the URL already has a `?jwt=`. Every other token rides its own AUTH stream on the
/// sessions of [`handle`](Self::handle), widening the grant. Replacing them presents the new
/// ones on the live session at once, withdrawing the ones they replace only after every new
/// one has its answer, so the grant never narrows in between.
///
/// A reconnect loop builds each session on [`handle`](Self::handle), calls
/// [`dial`](Self::dial) before each dial and [`connected`](Self::connected) once it
/// succeeds, and aborts the handle when it gives up.
///
/// Cheap to clone; every clone shares one set.
#[derive(Clone)]
pub struct Tokens {
	handle: Handle,
}

/// What one dial presents, from [`Tokens::dial`].
pub struct Dial {
	/// The URL to dial, carrying the connection credential in its query when an offered
	/// version has no SETUP token.
	pub url: Url,
	/// The connection credential for [`Client::with_token`](crate::Client::with_token), unless
	/// the URL brought its own.
	pub token: Option<crate::setup::Token>,
	generation: u64,
}

impl Tokens {
	/// Present `tokens` on the sessions of a new [`handle`](Self::handle).
	pub fn new(tokens: Vec<Token>) -> Self {
		let handle = Handle::new();
		handle.state.lock().configured = Some(Configured {
			fresh: dedup(&tokens),
			tokens,
			..Default::default()
		});
		Self { handle }
	}

	/// The handle to build each session on, with [`Client::with_auth`](crate::Client::with_auth).
	pub fn handle(&self) -> Handle {
		self.handle.clone()
	}

	/// Replace every token, reaching the live session at once and every later dial.
	pub fn set(&self, tokens: Vec<Token>) {
		let mut state = self.handle.state.lock();
		let configured = state.configured.as_ref().expect("built by Tokens::new");
		if configured.tokens == tokens {
			return;
		}
		// A token still waiting on its verdict stays fresh when the new set keeps it.
		let fresh = dedup(&tokens)
			.into_iter()
			.filter(|token| {
				!configured.tokens.contains(token)
					|| (configured.fresh.contains(token) && configured.verdict(&state, token).is_none())
			})
			.collect();
		let configured = state.configured.as_mut().expect("built by Tokens::new");
		configured.fresh = fresh;
		configured.tokens = tokens;
		configured.generation += 1;
		configured.answer = None;
		state.hold();
	}

	/// Poll for the peer's answer to the tokens set last; see [`answered`](Self::answered).
	pub fn poll_answered(&self, waiter: &kio::Waiter) -> Poll<Result<()>> {
		let answer = |state: &State| {
			state
				.configured
				.as_ref()
				.and_then(|configured| configured.answer(state))
		};
		let mut state = match self.handle.state.poll(waiter, |state| match answer(state) {
			Some(_) => Poll::Ready(()),
			None => Poll::Pending,
		}) {
			Poll::Ready(state) => state,
			Poll::Pending => return Poll::Pending,
		};
		state.record();
		Poll::Ready(answer(&state).expect("checked answered"))
	}

	/// Wait for the peer's first verdict on the tokens the last [`set`](Self::set) newly
	/// presented, or on the initial tokens before any set: `Ok` once each is granted, in band
	/// or as the connection credential a later dial carried.
	///
	/// A token the set kept, the connection credential included, counts only once the peer
	/// refuses it. Nothing is re-checked after the answer: a later revocation, or a new
	/// session that refuses a token an earlier one granted, does not change it.
	///
	/// # Errors
	///
	/// The peer's refusal of a token as [`Error::Session`], [`Error::Unsupported`] when the
	/// session cannot carry an in-band token, or the connection's end, such as a refused dial.
	#[cfg(feature = "async")]
	pub async fn answered(&self) -> Result<()> {
		kio::wait(|waiter| self.poll_answered(waiter)).await
	}

	/// Decide what a dial to `url` offering `versions` presents, and hold every other token in
	/// band on the session it builds. Call it before each dial.
	pub fn dial(&self, url: &Url, versions: &crate::Versions) -> Dial {
		let mut state = self.handle.state.lock();
		state.record();
		let configured = state.configured.as_mut().expect("built by Tokens::new");
		let (url, setup, connection) = credential(url, versions, &configured.tokens);
		let token = setup.map(|token| crate::setup::Token {
			kind: crate::setup::Token::OUT_OF_BAND,
			value: token.into_bytes(),
		});
		configured.dialed = Some(connection);
		configured.session = None;
		configured.unsupported = false;
		let generation = configured.generation;
		state.hold();
		Dial { url, token, generation }
	}

	/// The session the latest [`dial`](Self::dial) built on [`handle`](Self::handle), once it
	/// connected, whose own answer to the connection credential then counts. A session built
	/// on another handle answers nothing here.
	#[cfg(feature = "async")]
	pub fn connected(&self, session: &crate::Session) {
		if session.auth().state.same_channel(&self.handle.state) {
			self.connect(session.credential());
		}
	}

	fn connect(&self, credential: Option<&super::Auth>) {
		let mut state = self.handle.state.lock();
		state.record();
		let connected = match credential {
			Some(auth) => Some(Connected::Slot {
				link: auth.presented.link.expect("a session's own credential"),
				id: auth.presented.id,
			}),
			// The handle backs one open session at a time: the one just built, unless it ended.
			None => state
				.links
				.iter()
				.rev()
				.find(|(_, link)| link.closed.is_none())
				.map(|(link, _)| Connected::Accepted { link: *link }),
		};
		state.configured.as_mut().expect("built by Tokens::new").session = connected;
	}

	/// Whether the tokens changed since `dial` read them, so a refusal of its credential earns
	/// another dial.
	pub fn changed(&self, dial: &Dial) -> bool {
		let state = self.handle.state.read();
		state.configured.as_ref().expect("built by Tokens::new").generation != dial.generation
	}
}

/// The configured tokens a [`Tokens`] presents, kept on its handle's state so each verdict
/// a session records reaches them under the same lock.
#[derive(Default)]
pub(super) struct Configured {
	tokens: Vec<Token>,
	generation: u64,
	/// The latest dial's connection credential, never also held in band. `None` before the
	/// first dial.
	dialed: Option<Option<String>>,
	/// The tokens held on their own AUTH streams, by their id among the handle's tokens.
	held: Vec<(Token, u64)>,
	/// Tokens a change replaced, kept presented until every held token has its answer.
	retiring: Vec<u64>,
	/// The latest dial's session, once it connected.
	session: Option<Connected>,
	/// The tokens the latest sets newly presented whose verdict is not yet known.
	fresh: Vec<Token>,
	/// The first verdict on the latest set.
	answer: Option<Result<()>>,
	/// Whether the latest dial already logged that its session takes no tokens in band.
	unsupported: bool,
}

/// How the latest dial's session answers its connection credential.
enum Connected {
	/// The version has no AUTH, so the peer accepted it with its handshake, or with its
	/// SETUP when the session [awaits it](super::Link::await_setup).
	Accepted { link: u64 },
	/// Its own AUTH stream on that session.
	Slot { link: u64, id: u64 },
}

impl Configured {
	/// The latest dial's connection credential, when the tokens still name it.
	fn named(&self) -> Option<&str> {
		let connection = self.dialed.as_ref()?.as_deref()?;
		self.tokens
			.iter()
			.any(|token| token.as_str() == connection)
			.then_some(connection)
	}

	/// The peer's verdict on the connection credential, `None` while it has none or no
	/// session is live.
	fn credential(&self, state: &State) -> Option<Result<()>> {
		let (link, id) = match self.session.as_ref()? {
			Connected::Slot { link, id } => (link, id),
			Connected::Accepted { link } => {
				let link = state.links.get(link).filter(|link| link.closed.is_none())?;
				return link.accepted.then_some(Ok(()));
			}
		};
		let link = state.links.get(link).filter(|link| link.closed.is_none())?;
		let slot = link.slots.get(id)?;
		match (&slot.ended, &slot.answered) {
			// The moq-transport SETUP that carried it accepted it before the peer declined AUTH.
			(Some(Error::Unsupported), _) => Some(Ok(())),
			(Some(err), _) => Some(Err(err.clone())),
			(None, Some(Ok(()))) => Some(Ok(())),
			// Unanswered, or waiting out a refusal that may pass.
			(None, _) => None,
		}
	}

	/// The peer's verdict on a token the set newly presented, `None` while it has none.
	fn verdict(&self, state: &State, token: &Token) -> Option<Result<()>> {
		if let Some((_, id)) = self.held.iter().find(|(held, _)| held == token) {
			return match state.tokens.get(id)?.answered.clone()? {
				Ok(Answer::Granted) => Some(Ok(())),
				// Taken only for the requests that name it, and a configured token rides none.
				Ok(Answer::Requests) => Some(Err(Error::Unsupported)),
				Err(err) => Some(Err(err)),
			};
		}
		match self.named() == Some(token.as_str()) {
			true => self.credential(state),
			// Not presented until the first dial.
			false => None,
		}
	}

	/// The answer to the latest set: the first refusal of any token it names, or `Ok` once
	/// every fresh token is granted. `None` while one still waits.
	fn answer(&self, state: &State) -> Option<Result<()>> {
		if let Some(answer) = &self.answer {
			return Some(answer.clone());
		}
		if let Some(err) = &state.closed {
			return Some(Err(err.clone()));
		}
		let held = self
			.held
			.iter()
			.filter_map(|(_, id)| state.tokens.get(id)?.ended.clone());
		let credential = self.named().and_then(|_| self.credential(state)?.err());
		if let Some(err) = held.chain(credential).next() {
			return Some(Err(err));
		}
		let mut waiting = false;
		for token in &self.fresh {
			match self.verdict(state, token) {
				Some(Ok(())) => {}
				Some(Err(err)) => return Some(Err(err)),
				None => waiting = true,
			}
		}
		(!waiting).then_some(Ok(()))
	}
}

impl State {
	/// Whether token `id` is one a [`Tokens`] holds in band.
	pub(super) fn configured(&self, id: u64) -> bool {
		self.configured
			.as_ref()
			.is_some_and(|configured| configured.held.iter().any(|(_, held)| *held == id))
	}

	/// Configured token `id` rides nothing on this session, which the latest dial logs once.
	pub(super) fn unsupported(&mut self, id: u64) {
		if !self.configured(id) {
			return;
		}
		let configured = self.configured.as_mut().expect("checked configured");
		if !std::mem::replace(&mut configured.unsupported, true) {
			tracing::debug!("the peer takes no tokens in band; presenting only the connection credential");
		}
	}

	/// Keep each verdict the latest set has so far, before a dial, a new session, or a
	/// dropped one takes away where it came from.
	pub(super) fn record(&mut self) {
		let Some(configured) = &self.configured else { return };
		if configured.answer.is_some() {
			return;
		}
		let answer = configured.answer(self);
		let granted: Vec<Token> = configured
			.fresh
			.iter()
			.filter(|token| matches!(configured.verdict(self, token), Some(Ok(()))))
			.cloned()
			.collect();
		let configured = self.configured.as_mut().expect("checked configured");
		match answer {
			Some(answer) => {
				if answer.is_ok() {
					configured.fresh.clear();
				}
				configured.answer = Some(answer);
			}
			None => configured.fresh.retain(|token| !granted.contains(token)),
		}
	}

	/// Hold every configured token but the latest dial's connection credential in band,
	/// presenting the new ones and retiring the ones no longer configured.
	pub(super) fn hold(&mut self) {
		let Some(configured) = &self.configured else { return };
		let Some(connection) = configured.dialed.clone() else {
			return;
		};
		let tokens: Vec<Token> = dedup(&configured.tokens)
			.into_iter()
			.filter(|token| Some(token.as_str()) != connection.as_deref())
			.collect();
		let mut held = Vec::with_capacity(tokens.len());
		let mut presented = Vec::new();
		for token in tokens {
			let configured = self.configured.as_ref().expect("checked configured");
			let id = match configured.held.iter().find(|(kept, _)| *kept == token) {
				Some((_, id)) => *id,
				None => {
					let id = self.present(Bytes::from(token.as_str().to_string()));
					presented.push(id);
					id
				}
			};
			held.push((token, id));
		}
		let configured = self.configured.as_mut().expect("checked configured");
		let old = std::mem::replace(&mut configured.held, held);
		for (_, id) in old {
			if self.configured(id) {
				continue;
			}
			// A refused token has nothing left to keep the grant wide.
			match self.tokens.get(&id).is_some_and(|entry| entry.ended.is_none()) {
				true => self.configured.as_mut().expect("checked configured").retiring.push(id),
				false => self.withdraw(id),
			}
		}
		// A session without AUTH answers a new token at once, before it was held.
		for id in presented {
			let answered = self.tokens.get(&id).and_then(|entry| entry.answered.as_ref());
			if let Some(Ok(Answer::Requests) | Err(Error::Unsupported)) = answered {
				self.unsupported(id);
			}
		}
		self.settle();
	}

	/// Withdraw the replaced tokens once every held token has its answer.
	pub(super) fn settle(&mut self) {
		let Some(configured) = &self.configured else { return };
		if configured.retiring.is_empty()
			|| configured
				.held
				.iter()
				.any(|(_, id)| self.tokens.get(id).is_some_and(|entry| entry.answered.is_none()))
		{
			return;
		}
		let retiring = std::mem::take(&mut self.configured.as_mut().expect("checked configured").retiring);
		for id in retiring {
			self.withdraw(id);
		}
	}
}

/// Each of `tokens` once, in order.
fn dedup(tokens: &[Token]) -> Vec<Token> {
	let mut unique: Vec<Token> = Vec::with_capacity(tokens.len());
	for token in tokens {
		if !unique.contains(token) {
			unique.push(token.clone());
		}
	}
	unique
}

/// What a dial to `url` presents: the URL to dial, the token SETUP carries, and the
/// connection credential, which is never also held in band.
fn credential(url: &Url, versions: &crate::Versions, tokens: &[Token]) -> (Url, Option<String>, Option<String>) {
	if let Some(jwt) = jwt(url) {
		// The URL's own credential (a cluster's per-peer `?jwt=`) wins, so the tokens all
		// ride AUTH and no second credential reaches SETUP or the query.
		return (url.clone(), None, Some(jwt));
	}
	let Some(first) = tokens.first() else {
		return (url.clone(), None, None);
	};
	// The URL is fixed before the version is negotiated, so it carries the credential
	// whenever a version that may be picked could not carry it in SETUP.
	let url = match versions.iter().all(|version| version.has_setup_token()) {
		true => url.clone(),
		false => with_jwt(url, first.as_str()),
	};
	(url, Some(first.as_str().to_string()), Some(first.as_str().to_string()))
}

/// The request target's query: the URL's own, or for `unix:` the one inside `?path=`.
fn target_query(url: &Url) -> Option<String> {
	match url.scheme() {
		"unix" => url
			.query_pairs()
			.find(|(k, _)| k == "path")
			.and_then(|(_, path)| path.split_once('?').map(|(_, query)| query.to_string())),
		_ => url.query().map(str::to_string),
	}
}

/// The non-empty `jwt` the URL's request target already carries.
fn jwt(url: &Url) -> Option<String> {
	let query = target_query(url)?;
	url::form_urlencoded::parse(query.as_bytes())
		.find(|(k, v)| k == "jwt" && !v.is_empty())
		.map(|(_, v)| v.into_owned())
}

/// `url` with `jwt` appended to its request target's query.
fn with_jwt(url: &Url, jwt: &str) -> Url {
	let mut url = url.clone();
	if url.scheme() != "unix" {
		url.query_pairs_mut().append_pair("jwt", jwt);
		return url;
	}
	// A unix socket's request target is one form-encoded `?path=` value, so the
	// credential joins the query inside it.
	let pairs: Vec<(String, String)> = url.query_pairs().into_owned().collect();
	let mut serializer = url::form_urlencoded::Serializer::new(String::new());
	let mut found = false;
	for (k, v) in &pairs {
		match k == "path" {
			true => {
				found = true;
				let mut target = url::form_urlencoded::Serializer::new(String::new());
				target.append_pair("jwt", jwt);
				let separator = if v.contains('?') { '&' } else { '?' };
				serializer.append_pair(k, &format!("{v}{separator}{}", target.finish()));
			}
			false => {
				serializer.append_pair(k, v);
			}
		}
	}
	if !found {
		let mut target = url::form_urlencoded::Serializer::new(String::new());
		target.append_pair("jwt", jwt);
		serializer.append_pair("path", &format!("?{}", target.finish()));
	}
	url.set_query(Some(&serializer.finish()));
	url
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::{
		SessionError,
		auth::{Auth, Grant, Link},
	};

	fn tokens(names: &[&str]) -> Vec<Token> {
		names.iter().map(|name| name.parse().unwrap()).collect()
	}

	fn versions(names: &[&str]) -> crate::Versions {
		names
			.iter()
			.map(|name| name.parse::<crate::Version>().unwrap())
			.collect::<Vec<_>>()
			.into()
	}

	/// One session a dial built on the handle, with AUTH when `supported`, and moq-transport
	/// requests when `requests`.
	struct Session {
		link: Link,
		credential: Option<Auth>,
	}

	fn dial(set: &Tokens, supported: bool, requests: bool) -> Session {
		set.dial(
			&"moqt://relay.example.com/".parse().unwrap(),
			&versions(&["moq-lite-07-wip"]),
		);
		let link = Link::attach(&set.handle, supported, requests).unwrap();
		let credential = supported.then(|| link.present(Bytes::new(), true).unwrap());
		set.connect(credential.as_ref());
		Session { link, credential }
	}

	impl Session {
		fn credential(&self) -> u64 {
			self.credential.as_ref().unwrap().id()
		}

		fn grant_credential(&self) {
			self.link.granted(self.credential(), Grant::all());
		}

		/// The peer refuses or revokes the token presented as `id`, as the driver reports it.
		fn refuse(&self, id: u64, code: SessionError) {
			let err = Error::Session(code);
			self.link.refused(id);
			if self.link.retry(id, &err).is_none() {
				self.link.ended(id, err);
			}
		}

		fn lost(self) {
			self.link.close(Error::Transport("lost".into()));
		}
	}

	/// The id a configured token is held under.
	fn id(set: &Tokens, name: &str) -> u64 {
		let state = set.handle.state.read();
		let held = &state.configured.as_ref().unwrap().held;
		held.iter()
			.find(|(token, _)| token.as_str() == name)
			.expect("not held")
			.1
	}

	fn held(set: &Tokens) -> Vec<String> {
		let state = set.handle.state.read();
		let held = &state.configured.as_ref().unwrap().held;
		held.iter().map(|(token, _)| token.as_str().to_string()).collect()
	}

	fn presented(set: &Tokens, id: u64) -> bool {
		set.handle.state.read().tokens.contains_key(&id)
	}

	fn answered(set: &Tokens) -> Poll<Result<()>> {
		set.poll_answered(&kio::Waiter::noop())
	}

	fn ok(set: &Tokens) {
		let answer = answered(set);
		assert!(matches!(answer, Poll::Ready(Ok(()))), "{answer:?}");
	}

	fn refused(set: &Tokens, code: SessionError) {
		let answer = answered(set);
		assert!(
			matches!(&answer, Poll::Ready(Err(Error::Session(refused))) if *refused == code),
			"{answer:?}"
		);
	}

	/// Every token but the connection credential is held in band, each once.
	#[test]
	fn only_the_extra_tokens_ride_auth() {
		let set = Tokens::new(tokens(&["connection", "a", "b", "a"]));
		let _session = dial(&set, true, false);
		assert_eq!(held(&set), ["a", "b"]);
	}

	/// A replaced token is withdrawn only once its replacement has an answer, so the grant
	/// never narrows in between, and that answer is the set's.
	#[test]
	fn a_replacement_retires_what_it_replaced_once_answered() {
		let set = Tokens::new(tokens(&["connection", "old"]));
		let session = dial(&set, true, false);
		session.grant_credential();
		let old = id(&set, "old");
		session.link.granted(old, Grant::all());
		ok(&set);

		set.set(tokens(&["connection", "new"]));
		assert!(answered(&set).is_pending(), "answered before the replacement was");
		assert!(presented(&set, old), "withdrawn before its replacement was answered");

		session.link.granted(id(&set, "new"), Grant::all());
		ok(&set);
		assert!(!presented(&set, old), "the replaced token was never withdrawn");
	}

	/// A replacement is authoritative: its refusal is the answer, and still withdraws what it
	/// replaced.
	#[test]
	fn a_refused_replacement_still_retires_what_it_replaced() {
		let set = Tokens::new(tokens(&["connection", "old"]));
		let session = dial(&set, true, false);
		session.grant_credential();
		let old = id(&set, "old");
		session.link.granted(old, Grant::all());

		set.set(tokens(&["connection", "new"]));
		session.refuse(id(&set, "new"), SessionError::Unauthorized);
		refused(&set, SessionError::Unauthorized);
		assert!(
			!presented(&set, old),
			"the replaced token outlived its refused replacement"
		);
	}

	/// An extra token granted first is no answer to a fresh connection credential, whose own
	/// refusal is the answer even while its session stays open.
	#[test]
	fn the_connection_credential_answers_for_itself() {
		let set = Tokens::new(tokens(&["connection", "a"]));
		let session = dial(&set, true, false);
		session.link.granted(id(&set, "a"), Grant::all());
		assert!(answered(&set).is_pending(), "answered before the credential was");

		session.refuse(session.credential(), SessionError::Unauthorized);
		refused(&set, SessionError::Unauthorized);
	}

	/// A refusal that may pass presents the credential again, which answers nothing until
	/// the retry is answered.
	#[test]
	fn a_transient_refusal_of_the_credential_waits_for_its_retry() {
		let set = Tokens::new(tokens(&["connection"]));
		let session = dial(&set, true, false);
		session.refuse(session.credential(), SessionError::Internal);
		assert!(answered(&set).is_pending(), "answered before the retry was");

		session.link.reopen(session.credential()).unwrap();
		session.grant_credential();
		ok(&set);
	}

	/// The first verdict is the answer even when nothing polled it before a revocation or
	/// the connection's end.
	#[test]
	fn a_granted_set_keeps_its_answer_when_revoked_before_it_is_read() {
		let set = Tokens::new(tokens(&["connection", "a"]));
		let session = dial(&set, true, false);
		session.grant_credential();
		session.link.granted(id(&set, "a"), Grant::all());
		session.refuse(id(&set, "a"), SessionError::Unauthorized);
		ok(&set);

		set.set(tokens(&["connection", "b"]));
		session.link.granted(id(&set, "b"), Grant::all());
		set.handle().abort(Error::Session(SessionError::Unauthorized));
		ok(&set);
	}

	/// A session without AUTH that awaits the server's SETUP, as a moq-lite-05/06 client does.
	fn dial_awaiting_setup(set: &Tokens) -> Link {
		set.dial(
			&"moqt://relay.example.com/".parse().unwrap(),
			&versions(&["moq-lite-06"]),
		);
		let link = Link::attach(&set.handle, false, false).unwrap();
		link.await_setup();
		set.connect(None);
		link
	}

	/// Without AUTH, the server's SETUP is what accepts the connection credential.
	#[test]
	fn a_credential_without_auth_is_accepted_by_the_server_setup() {
		let set = Tokens::new(tokens(&["connection"]));
		let link = dial_awaiting_setup(&set);
		assert!(answered(&set).is_pending(), "accepted before the server's SETUP");
		link.accepted();
		ok(&set);
	}

	/// A server that refuses the credential before its SETUP grants nothing, even though
	/// the session was live.
	#[test]
	fn a_credential_refused_before_the_server_setup_is_no_grant() {
		let set = Tokens::new(tokens(&["connection"]));
		let link = dial_awaiting_setup(&set);
		link.close(Error::Session(SessionError::Unauthorized));
		assert!(answered(&set).is_pending(), "a refused session accepted the credential");
		set.handle().abort(Error::Session(SessionError::Unauthorized));
		refused(&set, SessionError::Unauthorized);
	}

	/// The connection credential revoked after its grant refuses a later set that keeps it.
	#[test]
	fn a_kept_credential_revoked_after_its_grant_refuses_the_next_set() {
		let set = Tokens::new(tokens(&["connection", "a", "b"]));
		let session = dial(&set, true, false);
		session.grant_credential();
		session.link.granted(id(&set, "a"), Grant::all());
		session.link.granted(id(&set, "b"), Grant::all());
		ok(&set);

		session.refuse(session.credential(), SessionError::Unauthorized);
		set.set(tokens(&["connection", "a"]));
		refused(&set, SessionError::Unauthorized);
	}

	/// A kept token revoked after its grant refuses a later set that keeps it.
	#[test]
	fn a_kept_token_revoked_after_its_grant_refuses_the_next_set() {
		let set = Tokens::new(tokens(&["connection", "a", "b"]));
		let session = dial(&set, true, false);
		session.grant_credential();
		session.link.granted(id(&set, "a"), Grant::all());
		session.link.granted(id(&set, "b"), Grant::all());
		ok(&set);

		session.refuse(id(&set, "a"), SessionError::Unauthorized);
		set.set(tokens(&["connection", "a"]));
		refused(&set, SessionError::Unauthorized);
	}

	/// A set that only keeps tokens answers at once after reconnecting to a moq-transport
	/// peer without AUTH, which grants nothing in band.
	#[test]
	fn a_kept_set_answers_at_once_on_a_session_without_auth() {
		let set = Tokens::new(tokens(&["connection", "a", "b"]));
		let first = dial(&set, true, false);
		first.grant_credential();
		first.link.granted(id(&set, "a"), Grant::all());
		first.link.granted(id(&set, "b"), Grant::all());
		first.lost();

		let _second = dial(&set, false, true);
		set.set(tokens(&["connection", "a"]));
		ok(&set);
	}

	/// A kept token counts only by refusal: one a moq-transport session without AUTH took
	/// only for requests is no refusal once a later session grants it.
	#[test]
	fn a_kept_token_is_judged_only_by_refusal() {
		let set = Tokens::new(tokens(&["connection", "a", "b"]));
		let first = dial(&set, false, true);
		let answer = answered(&set);
		assert!(
			matches!(answer, Poll::Ready(Err(Error::Unsupported))),
			"a session without AUTH carried a token in band: {answer:?}"
		);
		first.lost();

		let second = dial(&set, true, false);
		second.grant_credential();
		second.link.granted(id(&set, "a"), Grant::all());
		set.set(tokens(&["connection", "a"]));
		ok(&set);
	}

	/// A token set while offline becomes the next dial's connection credential, never also
	/// held in band, and that session's answer to it is the set's.
	#[test]
	fn a_set_while_offline_is_answered_by_the_next_credential() {
		let set = Tokens::new(tokens(&["old"]));
		let first = dial(&set, true, false);
		first.grant_credential();
		first.lost();

		set.set(tokens(&["new"]));
		assert!(answered(&set).is_pending(), "answered while offline");
		let second = dial(&set, true, false);
		assert!(held(&set).is_empty(), "the credential is also held in band");
		assert!(answered(&set).is_pending(), "answered before the credential was");
		second.grant_credential();
		ok(&set);
	}

	/// A token granted in band keeps its verdict when a later dial moves it to SETUP.
	#[test]
	fn a_granted_token_keeps_its_verdict_as_the_next_credential() {
		let set = Tokens::new(tokens(&["old"]));
		let first = dial(&set, true, false);
		first.grant_credential();
		set.set(tokens(&["new", "pending"]));
		first.link.granted(id(&set, "new"), Grant::all());
		first.lost();

		let second = dial(&set, true, false);
		assert_eq!(held(&set), ["pending"]);
		second.link.granted(id(&set, "pending"), Grant::all());
		ok(&set);
	}

	/// A session that takes no token in band answers a new one as unsupported.
	#[test]
	fn a_session_without_auth_is_unsupported() {
		for requests in [false, true] {
			let set = Tokens::new(tokens(&["connection"]));
			let _session = dial(&set, false, requests);
			ok(&set);
			set.set(tokens(&["connection", "a"]));
			let answer = answered(&set);
			assert!(
				matches!(answer, Poll::Ready(Err(Error::Unsupported))),
				"requests {requests}: {answer:?}"
			);
		}
	}

	/// The connection's end answers a set no session will.
	#[test]
	fn the_connection_end_answers_a_waiting_set() {
		let set = Tokens::new(tokens(&["connection"]));
		assert!(answered(&set).is_pending());
		set.handle().abort(Error::Session(SessionError::Unauthorized));
		refused(&set, SessionError::Unauthorized);
	}

	#[test]
	fn only_a_change_outdates_a_dial() {
		let set = Tokens::new(tokens(&["a"]));
		let dial = set.dial(
			&"moqt://relay.example.com/".parse().unwrap(),
			&versions(&["moq-lite-07-wip"]),
		);
		set.set(tokens(&["a"]));
		assert!(!set.changed(&dial));
		set.set(tokens(&["b"]));
		assert!(set.changed(&dial));
	}

	/// Only lite-07 and moq-transport carry the credential in SETUP, so the URL stays clean.
	#[test]
	fn setup_versions_keep_the_credential_out_of_the_url() {
		let url: Url = "https://relay.example.com/room".parse().unwrap();
		let (dialed, setup, connection) = credential(
			&url,
			&versions(&["moq-lite-07-wip", "moq-transport-17"]),
			&tokens(&["a", "b"]),
		);
		assert_eq!(dialed, url);
		assert_eq!(setup.as_deref(), Some("a"));
		assert_eq!(connection.as_deref(), Some("a"));
	}

	/// An offered version without a SETUP token needs the credential in the query, and the
	/// same value rides SETUP too.
	#[test]
	fn an_older_version_puts_the_credential_in_the_url() {
		let url: Url = "https://relay.example.com/room?x=1".parse().unwrap();
		let (dialed, setup, _) = credential(&url, &crate::Versions::all(), &tokens(&["a b"]));
		assert_eq!(dialed.as_str(), "https://relay.example.com/room?x=1&jwt=a+b");
		assert_eq!(setup.as_deref(), Some("a b"));
	}

	/// A URL credential stays the connection credential, so every token rides AUTH, except
	/// one equal to it, which would be granted twice.
	#[test]
	fn a_url_credential_sends_every_token_on_auth() {
		let url: Url = "moqt://relay.example.com/room?jwt=peer".parse().unwrap();
		let set = Tokens::new(tokens(&["a", "peer", "a"]));
		let dial = set.dial(&url, &crate::Versions::all());
		assert_eq!(dial.url, url);
		assert_eq!(dial.token, None);
		assert_eq!(held(&set), ["a"]);
	}

	#[test]
	fn no_tokens_present_nothing() {
		let url: Url = "https://relay.example.com/room".parse().unwrap();
		assert_eq!(credential(&url, &crate::Versions::all(), &[]), (url, None, None));
	}

	/// A unix socket's request target lives in `?path=`, so the credential goes inside it.
	#[test]
	fn a_unix_credential_joins_the_path_target() {
		for (url, expected) in [
			("unix:///tmp/moq.sock?path=%2Froom", "/room?jwt=a"),
			("unix:///tmp/moq.sock?path=%2Froom%3Fx%3D1", "/room?x=1&jwt=a"),
			("unix:///tmp/moq.sock", "?jwt=a"),
		] {
			let url: Url = url.parse().unwrap();
			let (dialed, _, _) = credential(&url, &crate::Versions::all(), &tokens(&["a"]));
			let path = dialed
				.query_pairs()
				.find(|(k, _)| k == "path")
				.map(|(_, v)| v.into_owned());
			assert_eq!(path.as_deref(), Some(expected), "{url}");
			// The rewritten target is read back as already carrying the credential.
			assert_eq!(jwt(&dialed).as_deref(), Some("a"));
		}
	}

	#[test]
	fn a_token_never_logs_its_value() {
		let token: Token = "secret".parse().unwrap();
		assert_eq!(format!("{token:?}"), "Token(<6 bytes>)");
		assert!("".parse::<Token>().is_err());
	}
}
