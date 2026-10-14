//! Tokens and grant watches that follow a reconnecting connection.

use std::task::{Poll, ready};

use bytes::Bytes;
use futures::FutureExt;
use moq_net::{auth, kio};

use super::{Pacing, State, terminal};
use crate::{Error, abort::AbortOnDrop};

/// Present session tokens across every session of a connection.
#[derive(Clone)]
pub struct Auth {
	pub(super) connection: kio::Consumer<State>,
	pub(super) pacing: Pacing,
	pub(super) request: RequestToken,
}

impl Auth {
	/// Present `token` as the `AUTHORIZATION TOKEN` on this connection's SUBSCRIBE and
	/// PUBLISH_NAMESPACE requests, now and after reconnects. Experimental: the peer's
	/// application must verify it, and success means only that the token was set.
	///
	/// Replacing it renews the live session's token-authorized requests in band, except a
	/// draft 14 or 15 advertisement, which has no update. Errors
	/// with [`moq_net::Error::Unsupported`] when the live session speaks moq-lite, whose
	/// requests carry no token; later sessions are still seeded with it.
	pub fn set_request_token(&self, token: moq_net::setup::Token) -> Result<(), moq_net::Error> {
		self.request.set(token)
	}

	/// Watch the current session's union, clearing it while disconnected.
	pub fn grant(&self) -> Watch {
		Watch {
			source: Source::Connection(self.connection.clone()),
			last: None,
		}
	}

	/// Present a token now and on later sessions until its handle is dropped or refused.
	pub async fn add(&self, token: impl Into<Bytes>) -> crate::Result<Token> {
		let producer = kio::Producer::<Granted>::default();
		let state = producer.consume();
		let connection = self.connection.clone();
		let pacing = self.pacing;
		let token = token.into();
		let (answered, answer) = tokio::sync::oneshot::channel();
		let task = AbortOnDrop::new(tokio::spawn(async move {
			let mut answered = Some(answered);
			if let Err(err) = present(connection, pacing, token, &producer, &mut answered).await {
				if let Some(answered) = answered.take() {
					let _ = answered.send(Err(err.clone()));
				}
				if let Ok(mut state) = producer.write() {
					state.grant = None;
					state.error = Some(err);
				}
			}
		}));
		answer.await.map_err(|_| Error::from(moq_net::Error::Cancel))??;
		Ok(Token { _task: task, state })
	}
}

/// The request token a connection presents and the session it presents it on, shared
/// across its sessions and shown by length only so it stays out of logs.
///
/// Seeding a new session ([`attach`](Self::attach)) and replacing the token
/// ([`set`](Self::set)) each read and write under one lock, so neither can leave a session
/// on an older token than the one set last, and a token set while a session is still
/// handshaking is the one it starts with. The lock is held only across synchronous work.
#[derive(Clone, Default)]
pub(crate) struct RequestToken(std::sync::Arc<std::sync::Mutex<Presenting>>);

#[derive(Default)]
struct Presenting {
	token: Option<moq_net::setup::Token>,
	/// The newest session, which a renewal reaches; `None` while nothing is live.
	session: Option<Box<dyn Present>>,
}

/// Where a request token is presented: a session's auth handle.
pub(crate) trait Present: Send {
	/// Present `token` on this session's requests from now on, or refuse it when the session
	/// cannot carry one (moq-lite).
	fn present(&self, token: moq_net::setup::Token) -> Result<(), moq_net::Error>;
}

impl Present for auth::Handle {
	fn present(&self, token: moq_net::setup::Token) -> Result<(), moq_net::Error> {
		self.set_request_token(token)
	}
}

impl RequestToken {
	pub(crate) fn new(token: Option<moq_net::setup::Token>) -> Self {
		Self(std::sync::Arc::new(std::sync::Mutex::new(Presenting {
			token,
			session: None,
		})))
	}

	/// Replace the token, on the newest session too, and seed every later session with it.
	///
	/// Errors when the newest session cannot carry it; later sessions are still seeded.
	fn set(&self, token: moq_net::setup::Token) -> Result<(), moq_net::Error> {
		let mut presenting = self.0.lock().unwrap();
		presenting.token = Some(token.clone());
		match &presenting.session {
			Some(session) => session.present(token),
			None => Ok(()),
		}
	}

	/// Seed `session` with the current token and make it the session later renewals reach.
	///
	/// Errors when the session cannot carry the token set, which leaves it unattached.
	pub(crate) fn attach(&self, session: impl Present + 'static) -> Result<(), moq_net::Error> {
		let mut presenting = self.0.lock().unwrap();
		if let Some(token) = &presenting.token {
			session.present(token.clone())?;
		}
		presenting.session = Some(Box::new(session));
		Ok(())
	}

	/// Stop reaching the session once nothing is live; renewals then only update the seed.
	pub(crate) fn detach(&self) {
		// Also runs from `Shared`'s drop, so take a poisoned lock rather than panic while unwinding.
		self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner).session = None;
	}
}

impl std::fmt::Debug for RequestToken {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		match &self.0.lock().unwrap().token {
			Some(token) => write!(f, "kind={} <{} bytes>", token.kind, token.value.len()),
			None => write!(f, "<none>"),
		}
	}
}

/// A presented token; dropping it withdraws it from live and draining sessions.
pub struct Token {
	_task: AbortOnDrop,
	state: kio::Consumer<Granted>,
}

impl Token {
	/// Watch this token's grant on the current session.
	pub fn grant(&self) -> Watch {
		Watch {
			source: Source::Token(self.state.clone()),
			last: None,
		}
	}

	/// Wait until the token is refused or its connection ends.
	pub async fn closed(&self) -> crate::Result<()> {
		kio::wait(|waiter| self.state.poll_closed(waiter)).await;
		match &self.state.read().error {
			Some(err) => Err(err.clone()),
			None => Ok(()),
		}
	}
}

#[derive(Default)]
struct Granted {
	grant: Option<auth::Grant>,
	error: Option<Error>,
}

#[derive(Clone)]
enum Source {
	Connection(kio::Consumer<State>),
	Token(kio::Consumer<Granted>),
}

/// Observe a grant as tokens change and the connection replaces its session.
#[derive(Clone)]
pub struct Watch {
	source: Source,
	last: Option<auth::Grant>,
}

impl Watch {
	/// The current grant, or none while disconnected or after the token ends.
	pub fn peek(&self) -> Option<auth::Grant> {
		match &self.source {
			Source::Connection(connection) => {
				if connection.poll_closed(&kio::Waiter::noop()).is_ready() {
					return None;
				}
				connection
					.read()
					.session
					.as_ref()
					.and_then(|session| session.auth().grant().peek())
			}
			Source::Token(state) => {
				if state.poll_closed(&kio::Waiter::noop()).is_ready() {
					return None;
				}
				state.read().grant.clone()
			}
		}
	}

	/// Poll for a different grant or the end of its owner.
	pub fn poll_changed(&mut self, waiter: &kio::Waiter) -> Poll<crate::Result<Option<auth::Grant>>> {
		let result = match &self.source {
			Source::Connection(connection) => match ready!(connection.poll(waiter, |state| {
				let mut watch = state.session.as_ref().map(|session| session.auth().grant());
				let current = watch.as_ref().and_then(auth::Watch::peek);
				if current != self.last {
					return Poll::Ready(current);
				}
				if let Some(watch) = &mut watch
					&& let Poll::Ready(Ok(grant)) = watch.poll_changed(waiter)
				{
					return Poll::Ready(grant);
				}
				Poll::Pending
			})) {
				Ok(grant) => Ok(grant),
				Err(state) => Err(terminal(&state)),
			},
			Source::Token(state) => match ready!(state.poll(waiter, |state| {
				if state.grant != self.last {
					Poll::Ready(state.grant.clone())
				} else {
					Poll::Pending
				}
			})) {
				Ok(grant) => Ok(grant),
				Err(state) => Err(state.error.clone().unwrap_or_else(|| moq_net::Error::Cancel.into())),
			},
		};
		if let Ok(grant) = &result {
			self.last = grant.clone();
		}
		Poll::Ready(result)
	}

	/// Wait for a different grant or the end of its owner.
	pub async fn changed(&mut self) -> crate::Result<Option<auth::Grant>> {
		kio::wait(|waiter| self.poll_changed(waiter)).await
	}
}

async fn next(
	connection: &kio::Consumer<State>,
	previous: Option<(u64, bool)>,
) -> crate::Result<(u64, Option<moq_net::Session>)> {
	kio::wait(|waiter| {
		match ready!(connection.poll(waiter, |state| {
			if Some((state.connects, state.session.is_some())) != previous {
				Poll::Ready((state.connects, state.session.clone()))
			} else {
				Poll::Pending
			}
		})) {
			Ok(session) => Poll::Ready(Ok(session)),
			Err(state) => Poll::Ready(Err(terminal(&state))),
		}
	})
	.await
}

/// Whether a refusal is the acceptor failing to decide rather than its verdict on
/// the token, such as an unreachable auth server or a full verification queue.
/// moq-transport's AUTH_ERROR carries neither, so there every refusal is final.
fn transient(err: &moq_net::Error) -> bool {
	matches!(
		err,
		moq_net::Error::Session(
			moq_net::SessionError::Internal | moq_net::SessionError::TooManyRequests | moq_net::SessionError::Timeout
		)
	)
}

async fn present(
	connection: kio::Consumer<State>,
	pacing: Pacing,
	bytes: Bytes,
	producer: &kio::Producer<Granted>,
	answered: &mut Option<tokio::sync::oneshot::Sender<crate::Result<()>>>,
) -> crate::Result<()> {
	let mut snapshot = next(&connection, None).await?;
	let mut draining = Vec::new();
	'session: loop {
		let (connects, session) = snapshot;
		let Some(session) = session else {
			if let Ok(mut state) = producer.write() {
				state.grant = None;
			}
			snapshot = next(&connection, Some((connects, false))).await?;
			continue;
		};
		if let Ok(mut state) = producer.write() {
			state.grant = None;
		}
		let handle = session.auth();
		let mut delay = None;
		let token = loop {
			let attempt = async {
				if let Some(delay) = delay {
					tokio::time::sleep(delay).await;
				}
				handle.add(bytes.clone()).await
			};
			let result = tokio::select! {
				biased;
				changed = next(&connection, Some((connects, true))) => { snapshot = changed?; continue 'session; }
				result = attempt => result,
			};
			match result {
				Ok(token) => break token,
				// The session ending is no verdict on the token: replay it on the next one.
				Err(_) if session.closed().now_or_never().is_some() => {
					snapshot = next(&connection, Some((connects, true))).await?;
					continue 'session;
				}
				// Nor is an acceptor that could not decide: present it again, paced like a
				// reconnect. Only an accepted grant counts, so a retry never extends one.
				Err(err) if transient(&err) => {
					let wait = delay.map_or(pacing.initial, |delay| pacing.next(delay));
					tracing::debug!(%err, ?wait, "token refused transiently; presenting it again");
					delay = Some(wait);
				}
				Err(err) => return Err(err.into()),
			}
		};
		if let Some(answered) = answered.take() {
			let _ = answered.send(Ok(()));
		}
		let mut watch = token.grant();
		if let Ok(mut state) = producer.write() {
			state.grant = watch.peek();
		}
		loop {
			tokio::select! {
				biased;
				changed = next(&connection, Some((connects, true))) => { snapshot = changed?; break; }
				_ = session.closed() => {
					if let Ok(mut state) = producer.write() { state.grant = None; }
					snapshot = next(&connection, Some((connects, true))).await?;
					break;
				}
				grant = watch.changed() => match grant {
					Ok(grant) => {
						if let Ok(mut state) = producer.write() { state.grant = grant; }
					}
					// The stream ended with its session, not by the peer's verdict.
					Err(_) if session.closed().now_or_never().is_some() => {
						if let Ok(mut state) = producer.write() { state.grant = None; }
						snapshot = next(&connection, Some((connects, true))).await?;
						break;
					}
					Err(err) => return Err(err.into()),
				}
			}
		}
		// A migrated session may still be serving its streams during GOAWAY drain.
		// Keep its grant until that session ends or the application drops this token.
		draining.retain(|(session, _): &(moq_net::Session, auth::Token)| session.closed().now_or_never().is_none());
		draining.push((session, token));
	}
}

#[cfg(test)]
mod request_token_tests {
	use super::*;

	impl RequestToken {
		fn get(&self) -> Option<moq_net::setup::Token> {
			self.0.lock().unwrap().token.clone()
		}
	}

	/// A request token of kind 0 carrying `value`.
	fn tok(value: &[u8]) -> moq_net::setup::Token {
		moq_net::setup::Token {
			kind: 0,
			value: value.to_vec(),
		}
	}

	/// The request token is shown by length only, never printed.
	#[test]
	fn request_token_debug_is_redacted() {
		let token = RequestToken::new(Some(tok(b"s3cr3t")));
		assert_eq!(format!("{token:?}"), "kind=0 <6 bytes>");
		assert_eq!(format!("{:?}", RequestToken::default()), "<none>");
	}

	/// A session double that records every token it is presented and parks inside its first
	/// [`Present::present`] until the test releases it: exactly where a seed or a renewal
	/// writes the session, after reading the token it writes.
	struct Parked {
		presented: std::sync::Arc<std::sync::Mutex<Vec<moq_net::setup::Token>>>,
		/// Taken by the first present, which reports that it parked and waits to be released.
		park: std::sync::Mutex<Option<(std::sync::mpsc::Sender<()>, std::sync::Arc<std::sync::Barrier>)>>,
	}

	impl Parked {
		fn new(presented: &std::sync::Arc<std::sync::Mutex<Vec<moq_net::setup::Token>>>) -> Self {
			Self {
				presented: presented.clone(),
				park: Default::default(),
			}
		}
	}

	impl Present for Parked {
		fn present(&self, token: moq_net::setup::Token) -> Result<(), moq_net::Error> {
			let park = self.park.lock().unwrap().take();
			if let Some((parked, release)) = park {
				parked.send(()).unwrap();
				release.wait();
			}
			self.presented.lock().unwrap().push(token);
			Ok(())
		}
	}

	/// Park the first present `write` makes on another thread, then race `renew` against it,
	/// returning every token the session was presented in order.
	///
	/// While `write` is parked mid-present, the cell's lock must still be held, so `renew`
	/// cannot land between `write`'s read and its write and be overwritten by the older token.
	fn race_renewal(
		token: &RequestToken,
		write: impl FnOnce(&RequestToken, Parked) + Send + 'static,
		renew: moq_net::setup::Token,
	) -> Vec<moq_net::setup::Token> {
		let presented = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
		let release = std::sync::Arc::new(std::sync::Barrier::new(2));
		let (step, steps) = std::sync::mpsc::channel();
		let parked = Parked::new(&presented);
		*parked.park.lock().unwrap() = Some((step, release.clone()));

		let writer = {
			let token = token.clone();
			std::thread::spawn(move || write(&token, parked))
		};
		steps.recv().expect("the write under test never presented a token");
		assert!(
			token.0.try_lock().is_err(),
			"a renewal could land between the parked write's read and its write"
		);

		let renewer = {
			let token = token.clone();
			std::thread::spawn(move || token.set(renew).unwrap())
		};
		release.wait();
		writer.join().expect("writer panicked");
		renewer.join().expect("renewer panicked");
		std::mem::take(&mut *presented.lock().unwrap())
	}

	/// A session that cannot carry a request token (moq-lite).
	struct Refuses;

	impl Present for Refuses {
		fn present(&self, _token: moq_net::setup::Token) -> Result<(), moq_net::Error> {
			Err(moq_net::Error::Unsupported)
		}
	}

	/// A session that cannot carry the token refuses it loudly: attaching fails, so the dial
	/// does too, and a renewal reaching it fails while still seeding later sessions.
	#[test]
	fn a_session_that_cannot_carry_the_token_refuses_it() {
		let token = RequestToken::new(Some(tok(b"token-a")));
		assert!(matches!(token.attach(Refuses), Err(moq_net::Error::Unsupported)));

		let token = RequestToken::new(None);
		token.attach(Refuses).expect("nothing to present");
		assert!(matches!(token.set(tok(b"token-b")), Err(moq_net::Error::Unsupported)));
		assert_eq!(token.get(), Some(tok(b"token-b")), "later sessions are still seeded");
	}

	/// A reconnect seeding its new session cannot replay the token it read over a renewal that
	/// lands meanwhile: the session ends on the renewal, as the reconnect seed does.
	#[test]
	fn a_renewal_racing_the_reconnect_seed_is_not_replayed() {
		let a = tok(b"token-a");
		let b = tok(b"token-b");
		let token = RequestToken::new(Some(a.clone()));

		let presented = race_renewal(&token, |token, session| token.attach(session).unwrap(), b.clone());

		assert_eq!(presented, [a, b.clone()], "seeded with A, then renewed to B");
		assert_eq!(token.get(), Some(b), "the next reconnect is seeded with B too");
	}

	/// Two renewals present in the order they land, so the session ends on the one that landed
	/// last, the same token the next reconnect is seeded with.
	#[test]
	fn concurrent_renewals_leave_the_session_on_the_last() {
		let a = tok(b"token-a");
		let b = tok(b"token-b");
		let token = RequestToken::new(None);

		let presented = race_renewal(
			&token,
			{
				let a = a.clone();
				move |token, session| {
					// Nothing to seed, so the attach presents nothing and the renewal parks.
					token.attach(session).unwrap();
					token.set(a).unwrap();
				}
			},
			b.clone(),
		);

		assert_eq!(presented, [a, b.clone()], "renewed to A, then to B");
		assert_eq!(token.get(), Some(b), "the next reconnect is seeded with B too");
	}

	/// Once nothing is live a renewal only updates the seed, and the next attach picks it up.
	#[test]
	fn a_renewal_between_sessions_seeds_the_next() {
		let presented = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
		let session = || Parked::new(&presented);
		let token = RequestToken::new(Some(tok(b"token-a")));

		token.attach(session()).unwrap();
		token.detach();
		token.set(tok(b"token-b")).unwrap();
		assert_eq!(
			presented.lock().unwrap().len(),
			1,
			"a renewal reached a detached session"
		);

		token.attach(session()).unwrap();
		assert_eq!(presented.lock().unwrap().last().cloned(), Some(tok(b"token-b")));
	}
}

#[cfg(all(test, feature = "tcp"))]
mod tests {
	use super::*;

	struct Pair {
		client: moq_net::Session,
		requests: tokio::sync::mpsc::UnboundedReceiver<auth::Request>,
		_tasks: Vec<AbortOnDrop<moq_net::Error>>,
		_acceptor: AbortOnDrop,
	}

	async fn pair() -> Pair {
		let versions = moq_net::Versions::from("moq-lite-07-wip".parse::<moq_net::Version>().unwrap());
		let config = || {
			let mut config = qmux::Config::new(qmux::Version::QMux01);
			config.protocol = qmux::Protocol::Negotiate(versions.alpns().iter().map(|alpn| alpn.to_string()).collect());
			config
		};
		let stream = |io| qmux::transport::Stream::new(io, qmux::Version::QMux01, config().max_record_size);
		let (client, server) = tokio::io::duplex(64 * 1024);
		let (client, server) = tokio::try_join!(
			qmux::Session::connect(stream(client), config()),
			qmux::Session::accept(stream(server), config())
		)
		.unwrap();
		let now = tokio::time::Instant::now().into_std();
		let client = async {
			let (session, driver) = moq_net::Client::new()
				.with_versions(versions.clone())
				.connect(now, crate::transport::Session::new(client))
				.await
				.unwrap();
			(session, AbortOnDrop::new(tokio::spawn(moq_net::time::run(driver))))
		};
		let server = async {
			let request = moq_net::Server::new()
				.with_versions(versions.clone())
				.accept_request(now, crate::transport::Session::new(server))
				.await
				.unwrap();
			let mut requests = request.auth().requests().unwrap();
			let (session, driver) = request.ok().await.unwrap();
			let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
			let acceptor = tokio::spawn(async move {
				let _session = session;
				let mut initial = Vec::new();
				while let Some(auth::Event::Session(request)) = requests.next().await {
					if request.token().is_empty() {
						initial.push(request.accept(auth::Grant::default()));
					} else if sender.send(request).is_err() {
						break;
					}
				}
			});
			(
				receiver,
				AbortOnDrop::new(tokio::spawn(moq_net::time::run(driver))),
				AbortOnDrop::new(acceptor),
			)
		};
		let ((client, client_task), (requests, server_task, acceptor)) = tokio::join!(client, server);
		Pair {
			client,
			requests,
			_tasks: vec![client_task, server_task],
			_acceptor: acceptor,
		}
	}

	fn attach(connection: &kio::Producer<State>, session: &moq_net::Session) {
		let mut state = connection.write().ok().expect("connection observed");
		state.connects += 1;
		state.session = Some(session.clone());
	}

	#[tokio::test(start_paused = true)]
	async fn a_token_survives_replacement_and_withdraws_from_both_sessions() {
		let connection = kio::Producer::<State>::default();
		let auth = retained(&connection);
		let mut first = pair().await;
		attach(&connection, &first.client);
		let (token, first_grant) = tokio::join!(auth.add("renew"), async {
			let request = first.requests.recv().await.unwrap();
			assert_eq!(request.token().as_ref(), b"renew");
			request.accept(auth::Grant::all())
		});
		let token = token.unwrap();
		let mut grant = token.grant();
		assert!(grant.changed().await.unwrap().is_some());
		let mut second = pair().await;
		attach(&connection, &second.client);
		let request = second.requests.recv().await.unwrap();
		assert_eq!(request.token().as_ref(), b"renew");
		assert!(
			grant.changed().await.unwrap().is_none(),
			"replacement reused its predecessor grant"
		);
		let second_grant = request.accept(auth::Grant::all());
		assert!(grant.changed().await.unwrap().is_some());
		assert!(
			first_grant.closed().now_or_never().is_none(),
			"migration withdrew a draining grant"
		);
		drop(token);
		let _ = tokio::join!(first_grant.closed(), second_grant.closed());
	}

	#[tokio::test(start_paused = true)]
	async fn a_session_closing_mid_replay_keeps_the_token() {
		let connection = kio::Producer::<State>::default();
		let auth = retained(&connection);
		let mut first = pair().await;
		attach(&connection, &first.client);
		let (token, _first_grant) = tokio::join!(auth.add("renew"), async {
			first.requests.recv().await.unwrap().accept(auth::Grant::all())
		});
		let token = token.unwrap();
		let mut second = pair().await;
		attach(&connection, &second.client);
		// Close the replacement while its replayed token is still unanswered, before
		// the connection notices.
		let _unanswered = second.requests.recv().await.unwrap();
		second.client.abort(moq_net::Error::Cancel);
		second.client.closed().await;
		let mut third = pair().await;
		attach(&connection, &third.client);
		let request = tokio::time::timeout(std::time::Duration::from_secs(5), third.requests.recv())
			.await
			.expect("the token was not replayed after its session closed")
			.unwrap();
		assert_eq!(request.token().as_ref(), b"renew");
		drop(token);
	}

	fn retained(connection: &kio::Producer<State>) -> Auth {
		Auth {
			connection: connection.consume(),
			pacing: Pacing::new(&super::super::Backoff::default()),
			request: RequestToken::default(),
		}
	}

	#[tokio::test(start_paused = true)]
	async fn a_transient_refusal_is_presented_again_after_backoff() {
		let connection = kio::Producer::<State>::default();
		let auth = retained(&connection);
		let mut pair = pair().await;
		attach(&connection, &pair.client);
		let start = tokio::time::Instant::now();
		let (token, issued) = tokio::join!(auth.add("renew"), async {
			for _ in 0..2 {
				let outage = pair.requests.recv().await.unwrap();
				outage.reject(moq_net::SessionError::Internal, "auth server unavailable");
			}
			let retried = pair.requests.recv().await.unwrap();
			assert_eq!(retried.token().as_ref(), b"renew");
			retried.accept(auth::Grant::all())
		});
		let token = token.unwrap();
		// The default pacing waits 1s, then 2s, before the third presentation.
		assert!(
			start.elapsed() >= std::time::Duration::from_secs(3),
			"retried without backing off: {:?}",
			start.elapsed()
		);
		assert!(token.grant().changed().await.unwrap().is_some());
		drop(token);
		issued.closed().await;
	}

	#[tokio::test(start_paused = true)]
	async fn an_unauthorized_refusal_is_final() {
		let connection = kio::Producer::<State>::default();
		let auth = retained(&connection);
		let mut pair = pair().await;
		attach(&connection, &pair.client);
		let (token, ()) = tokio::join!(auth.add("renew"), async {
			let refused = pair.requests.recv().await.unwrap();
			refused.reject(moq_net::SessionError::Unauthorized, "token refused");
		});
		assert!(token.is_err());
		tokio::time::sleep(std::time::Duration::from_secs(60)).await;
		assert!(pair.requests.try_recv().is_err(), "a refused token was presented again");
	}

	#[tokio::test(start_paused = true)]
	async fn ending_the_connection_withdraws_a_retained_token_handle() {
		let connection = kio::Producer::<State>::default();
		let auth = retained(&connection);
		let mut pair = pair().await;
		attach(&connection, &pair.client);
		let (token, issued) = tokio::join!(auth.add("renew"), async {
			pair.requests.recv().await.unwrap().accept(auth::Grant::all())
		});
		let token = token.unwrap();
		drop(connection);
		assert!(token.closed().await.is_err());
		assert!(token.grant().peek().is_none());
		issued.closed().await;
	}
}
