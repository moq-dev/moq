//! Tokens and grant watches that follow a reconnecting connection.

use std::{
	sync::{Arc, Mutex},
	task::{Poll, ready},
};

use bytes::Bytes;
use futures::FutureExt;
use moq_net::{auth, kio};

use super::{Pacing, State, terminal};
use crate::{Error, abort::AbortOnDrop};

/// Present session tokens across every session of a connection.
#[derive(Clone)]
pub struct Handle {
	pub(super) connection: kio::Consumer<State>,
	pub(super) pacing: Pacing,
}

impl Handle {
	/// Watch the current session's union, clearing it while disconnected.
	pub fn grant(&self) -> Watch {
		Watch {
			source: Source::Connection(self.connection.clone()),
			last: None,
		}
	}

	/// Present a token now and on later sessions until its handle is dropped or refused.
	///
	/// A session that cannot send it, without AUTH, refuses it as unsupported.
	pub async fn add(&self, token: impl Into<Bytes>) -> crate::Result<Auth> {
		let producer = kio::Producer::<Granted>::default();
		let state = producer.consume();
		let connection = self.connection.clone();
		let pacing = self.pacing;
		let token = token.into();
		let replay = Arc::new(Replay {
			bytes: token,
			queued: Mutex::new(Vec::new()),
		});
		{
			let state = connection.read();
			let mut tokens = state.tokens.lock().unwrap();
			tokens.retain(|token| token.strong_count() > 0);
			if let Some(session) = &state.session {
				replay.queue(session, state.connects);
			}
			tokens.push(Arc::downgrade(&replay));
		}
		let (answered, answer) = tokio::sync::oneshot::channel();
		let task = AbortOnDrop::new(tokio::spawn(async move {
			let mut answered = Some(answered);
			if let Err(err) = present(connection, pacing, replay, &producer, &mut answered).await {
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
		Ok(Auth { _task: task, state })
	}
}

type Opening = futures::future::BoxFuture<'static, moq_net::Result<auth::Auth>>;

/// Registration shared with session publication, while the task owns its token's life.
pub(super) struct Replay {
	bytes: Bytes,
	/// Registrations queued as sessions are published, until the task takes them.
	queued: Mutex<Vec<Queued>>,
}

struct Queued {
	connects: u64,
	session: moq_net::Session,
	opening: Opening,
}

impl Replay {
	fn queue(&self, session: &moq_net::Session, connects: u64) {
		let handle = session.auth();
		let bytes = self.bytes.clone();
		let mut opening = async move { handle.add(bytes).await }.boxed();
		// The first poll registers the token synchronously, before the session can be
		// observed. The replay task replaces this noop waker when it takes the future.
		if let Some(result) = opening.as_mut().now_or_never() {
			opening = futures::future::ready(result).boxed();
		}
		self.queued.lock().unwrap().push(Queued {
			connects,
			session: session.clone(),
			opening,
		});
	}

	/// Take the registration queued for session `connects`, moving older ones to `draining`.
	fn take(&self, connects: u64, draining: &mut Draining) -> Option<Opening> {
		let mut queued = self.queued.lock().unwrap();
		let mut current = None;
		for entry in std::mem::take(&mut *queued) {
			match entry.connects.cmp(&connects) {
				// Requests on a draining session may still be waiting on this answer.
				std::cmp::Ordering::Less => draining.push(entry.session, entry.opening),
				std::cmp::Ordering::Equal => current = Some(entry.opening),
				// A replacement published since the task's snapshot already holds this token.
				// Dropping its registration would withdraw the token from a session consumers
				// can see, so leave it for the task's next pass.
				std::cmp::Ordering::Greater => queued.push(entry),
			}
		}
		current
	}
}

/// Tokens kept on sessions draining after a migration. Each is held by a task that
/// releases it, and its session, once that session ends; dropping this withdraws them all.
#[derive(Default)]
struct Draining(Vec<AbortOnDrop>);

impl Draining {
	/// Keep `held`, a token answered or not, registered on `session` until it ends.
	fn push(&mut self, session: moq_net::Session, held: impl Send + 'static) {
		self.0.retain(|task| !task.is_finished());
		self.0.push(AbortOnDrop::new(tokio::spawn(async move {
			session.closed().await;
			drop(held);
		})));
	}
}

pub(super) fn replay(state: &State, session: &moq_net::Session, connects: u64) {
	state.tokens.lock().unwrap().retain(|token| {
		let Some(token) = token.upgrade() else {
			return false;
		};
		token.queue(session, connects);
		true
	});
}

/// A presented token; dropping it withdraws it from live and draining sessions.
pub struct Auth {
	_task: AbortOnDrop,
	state: kio::Consumer<Granted>,
}

impl Auth {
	/// Watch this token's grant on the current session.
	pub fn grant(&self) -> Watch {
		Watch {
			source: Source::Auth(self.state.clone()),
			last: None,
		}
	}

	/// Wait until the token ends: `Ok` when the peer closed it without an error, otherwise its
	/// refusal or revocation, or the connection's end.
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
	Auth(kio::Consumer<Granted>),
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
			Source::Auth(state) => {
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
			Source::Auth(state) => match ready!(state.poll(waiter, |state| {
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
	replay: Arc<Replay>,
	producer: &kio::Producer<Granted>,
	answered: &mut Option<tokio::sync::oneshot::Sender<crate::Result<()>>>,
) -> crate::Result<()> {
	let mut snapshot = next(&connection, None).await?;
	let mut draining = Draining::default();
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
		let mut delay = None;
		let mut opening = replay.take(connects, &mut draining);
		let token = loop {
			let attempt = async {
				if opening.is_none() {
					if let Some(delay) = delay {
						moq_clock::sleep(delay).await;
					}
					let handle = session.auth();
					let bytes = replay.bytes.clone();
					opening = Some(async move { handle.add(bytes).await }.boxed());
				}
				opening.as_mut().expect("opening").await
			};
			let result = tokio::select! {
				biased;
				changed = next(&connection, Some((connects, true))) => Err(changed),
				result = attempt => Ok(result),
			};
			let result = match result {
				Ok(result) => result,
				Err(changed) => {
					// Requests on a draining session may still be waiting on this answer.
					if let Some(opening) = opening.take() {
						draining.push(session, opening);
					}
					snapshot = changed?;
					continue 'session;
				}
			};
			opening = None;
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
		let mut watch = token.grant();
		// Without AUTH, moq-transport accepts a token at once for the requests that name it,
		// but a connection's token rides no request, so nothing would send it. One the peer
		// already ended has a grant no longer and is left to the watch below.
		if watch.peek().is_none() && token.closed().now_or_never().is_none() {
			return Err(moq_net::Error::Unsupported.into());
		}
		if let Ok(mut state) = producer.write() {
			state.grant = watch.peek();
		}
		// After the grant, so `add` never returns a token that still reads none.
		if let Some(answered) = answered.take() {
			let _ = answered.send(Ok(()));
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
					// The peer closed it without an error: the token is over, not refused.
					Err(moq_net::Error::Cancel) => return Ok(()),
					Err(err) => return Err(err.into()),
				}
			}
		}
		// A migrated session may still be serving its streams during GOAWAY drain.
		// Keep its grant until that session ends or the application drops this token.
		draining.push(session, token);
	}
}

#[cfg(all(test, feature = "tcp"))]
#[expect(
	clippy::disallowed_methods,
	clippy::disallowed_types,
	reason = "tests read the host clock directly"
)]
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
		let stream = |io| qmux::transport::Stream::new(io);
		let (client, server) = tokio::io::duplex(64 * 1024);
		let (client, server) = tokio::try_join!(
			qmux::Session::connect(stream(client), config()),
			qmux::Session::accept(stream(server), config())
		)
		.unwrap();
		let now = moq_net::time::now();
		let client = async {
			let (session, driver) = moq_net::Client::new()
				.with_versions(versions.clone())
				.connect(now, client)
				.await
				.unwrap();
			(session, AbortOnDrop::new(tokio::spawn(moq_net::time::run(driver))))
		};
		let server = async {
			let request = moq_net::Server::new()
				.with_versions(versions.clone())
				.accept_request(now, server)
				.await
				.unwrap();
			let mut requests = request.auth().requests().unwrap();
			let (session, driver) = request.ok().await.unwrap();
			let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
			let acceptor = tokio::spawn(async move {
				let _session = session;
				let mut initial = Vec::new();
				while let Some(request) = requests.next().await {
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
		replay(&state, session, state.connects);
		state.session = Some(session.clone());
	}

	#[tokio::test(start_paused = true)]
	async fn replay_is_registered_before_the_session_is_published() {
		let connection = kio::Producer::<State>::default();
		let auth = retained(&connection);
		let mut first = pair().await;
		attach(&connection, &first.client);
		let (token, _issued) = tokio::join!(auth.add("replay"), async {
			first.requests.recv().await.unwrap().accept(auth::Grant::all())
		});
		let token = token.unwrap();
		let mut second = pair().await;
		let connects = {
			let state = connection.read();
			let connects = state.connects + 1;
			replay(&state, &second.client, connects);
			connects
		};
		// The task still observes the first session. The second token reaches the peer
		// before publication only if queueing it already polled add's registration.
		let request = tokio::time::timeout(std::time::Duration::from_secs(1), second.requests.recv())
			.await
			.expect("token was not queued before publication")
			.unwrap();
		assert_eq!(request.token().as_ref(), b"replay");
		{
			let mut state = connection.write().ok().expect("connection observed");
			state.connects = connects;
			state.session = Some(second.client.clone());
		}
		let _issued = request.accept(auth::Grant::all());
		let mut watch = token.grant();
		while watch.peek().is_none() {
			watch.changed().await.unwrap();
		}
	}

	#[tokio::test(start_paused = true)]
	async fn a_stale_snapshot_keeps_the_newer_sessions_registration() {
		let mut first = pair().await;
		let mut second = pair().await;
		let replay = Replay {
			bytes: "replay".into(),
			queued: Mutex::new(Vec::new()),
		};
		let mut draining = Draining::default();
		// The replacement queues and publishes while the task still holds the first snapshot.
		replay.queue(&second.client, 2);
		let request = second.requests.recv().await.unwrap();
		assert!(replay.take(1, &mut draining).is_none());
		let opening = replay
			.take(2, &mut draining)
			.expect("the newer session's registration was dropped");
		let _issued = request.accept(auth::Grant::all());
		let _token = opening.await.unwrap();
		// A registration for a session the task has moved past stays on it while it drains.
		replay.queue(&first.client, 1);
		let _stale = first.requests.recv().await.unwrap();
		assert!(replay.take(2, &mut draining).is_none());
		assert!(replay.queued.lock().unwrap().is_empty());
		assert_eq!(draining.0.len(), 1, "the draining session lost its registration");
	}

	/// A draining session that closes releases its token and its session handle at once,
	/// not only when a later migration adds another.
	#[tokio::test(start_paused = true)]
	async fn a_closed_draining_session_releases_its_token() {
		let pair = pair().await;
		let mut draining = Draining::default();
		let held = Arc::new(());
		draining.push(pair.client.clone(), held.clone());
		tokio::time::sleep(std::time::Duration::from_millis(10)).await;
		assert_eq!(Arc::strong_count(&held), 2, "released while its session lives");
		pair.client.abort(moq_net::Error::Cancel);
		pair.client.closed().await;
		tokio::time::sleep(std::time::Duration::from_millis(10)).await;
		assert_eq!(Arc::strong_count(&held), 1, "a closed draining session kept its token");
		drop(draining);
	}

	/// A migration keeps an unanswered replay on the session it leaves, whether or not the
	/// task had taken it, since requests there may still be waiting on its answer.
	#[tokio::test(start_paused = true)]
	async fn a_draining_session_keeps_an_unanswered_replay() {
		for taken in [false, true] {
			let connection = kio::Producer::<State>::default();
			let auth = retained(&connection);
			let mut first = pair().await;
			attach(&connection, &first.client);
			let (token, _first_grant) = tokio::join!(auth.add("renew"), async {
				first.requests.recv().await.unwrap().accept(auth::Grant::all())
			});
			let token = token.unwrap();
			let mut second = pair().await;
			let third = pair().await;
			attach(&connection, &second.client);
			if taken {
				// Let the task take the queued registration and wait on its answer.
				tokio::time::sleep(std::time::Duration::from_millis(10)).await;
			}
			attach(&connection, &third.client);
			let request = tokio::time::timeout(std::time::Duration::from_secs(5), second.requests.recv())
				.await
				.unwrap_or_else(|_| panic!("taken={taken}: the draining session withdrew its token unsent"))
				.unwrap();
			assert_eq!(request.token().as_ref(), b"renew");
			let _issued = request.accept(auth::Grant::all());
			let mut grant = second.client.auth().grant();
			tokio::time::timeout(std::time::Duration::from_secs(5), async {
				while grant.peek() != Some(auth::Grant::all()) {
					grant.changed().await.unwrap();
				}
			})
			.await
			.unwrap_or_else(|_| panic!("taken={taken}: the draining session never applied its token"));
			tokio::time::sleep(std::time::Duration::from_secs(1)).await;
			assert_eq!(
				second.client.auth().grant().peek(),
				Some(auth::Grant::all()),
				"taken={taken}: the draining session withdrew its token"
			);
			drop(token);
		}
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

	fn retained(connection: &kio::Producer<State>) -> Handle {
		Handle {
			connection: connection.consume(),
			pacing: Pacing::new(&super::super::Backoff::default()),
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
