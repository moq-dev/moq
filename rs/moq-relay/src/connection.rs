use crate::{auth, cluster, incoming::Incoming, refusals::RefusalKind};

use axum::http;
use tracing::Instrument;

/// An error carrying the HTTP status to send when closing the request.
///
/// Used only on the pre-accept auth path so the caller can close once with
/// the right code instead of sprinkling close/return at each failure site.
pub(crate) struct StatusError {
	pub(crate) status: http::StatusCode,
	refusal: Option<RefusalKind>,
	pub(crate) source: anyhow::Error,
}

impl From<auth::Error> for StatusError {
	fn from(err: auth::Error) -> Self {
		Self {
			status: (&err).into(),
			refusal: Some((&err).into()),
			source: err.into(),
		}
	}
}

/// An incoming connection that has not yet been admitted.
///
/// Build with [`new`](Self::new), attach the optional knobs, then call
/// [`run`](Self::run) to admit the request through its lease, wire up
/// publish/subscribe origins, and serve the session until it closes.
///
/// The request is a moq-tokio [`Request`](moq_tokio::server::Request), or an
/// in-memory accepted session when the `test-support` feature is enabled.
pub struct Connection<R: Incoming = moq_tokio::server::Request> {
	request: R,
	lifecycle: Lifecycle,
}

/// Shared owner of admission, supervision and the connection's log span.
#[derive(Clone)]
pub(crate) struct Lifecycle {
	id: u64,
	span: tracing::Span,
	cluster: cluster::Cluster,
	auth: auth::Auth,
	shutdown: crate::shutdown::Observer,
	sessions: Option<crate::session::Registry>,
}

impl<R: Incoming> Connection<R> {
	/// Wrap an accepted request, resolving origins through `cluster` and
	/// its grant through `auth`.
	pub fn new(request: R, cluster: cluster::Cluster, auth: auth::Auth) -> Self {
		Self {
			lifecycle: Lifecycle::new(cluster, auth, 0, request.remote()),
			request,
		}
	}

	/// Set the identifier this connection logs under. Defaults to 0.
	pub fn with_id(mut self, id: u64) -> Self {
		self.lifecycle.id = id;
		self.lifecycle.span = span(id, self.request.remote());
		self
	}

	/// Attach the relay-wide shutdown broadcast so the session drains with a
	/// GOAWAY when it fires. Without it the session is cut off on process exit.
	pub fn with_shutdown(mut self, shutdown: crate::shutdown::Observer) -> Self {
		self.lifecycle = self.lifecycle.with_shutdown(shutdown);
		self
	}

	/// Register this session in the node's live table after it is admitted.
	/// Without it the session is served but cannot be listed or nudged.
	pub fn with_sessions(mut self, sessions: crate::session::Registry) -> Self {
		self.lifecycle = self.lifecycle.with_sessions(sessions);
		self
	}

	/// Admits and serves this connection until it closes, then logs the close. A panic
	/// ends only this connection, as an error.
	pub async fn run(self) {
		let Self { request, lifecycle } = self;
		lifecycle.run(Self::serve(request, &lifecycle)).await;
	}

	async fn serve(request: R, lifecycle: &Lifecycle) -> anyhow::Result<()> {
		// Abandoned at the drain deadline, so the deadline bounds this task and its
		// socket like it bounds a session, and at the handshake deadline, so a peer
		// cannot hold the connection open behind a decision that never comes.
		// Boxed: the admission future is large, and nesting it inline overflows debug stacks.
		let admitted = request
			.within(Box::pin(lifecycle.admit(request.admission(&lifecycle.auth))))
			.await
			.inspect_err(|_| lifecycle.cluster.refusals.record(RefusalKind::Unavailable))
			.map_err(|err| err.context("admission timed out"))?;
		let (admitted, registration) = match admitted {
			Ok(admitted) => admitted,
			Err(err) => {
				request.reject(err.status).await;
				return Err(err.source);
			}
		};

		let transport = request.transport();
		let cluster::Admitted {
			lease,
			publisher,
			subscriber,
			stats,
		} = admitted;

		// A peer's QUIC limits widen before SETUP OK, so its first requests see them.
		// Other transports keep their own limits.
		#[cfg(feature = "quic")]
		if let Some(limits) = lifecycle.cluster.quic_limits(&lease)
			&& let Some(quic) = request.quic()
		{
			quic.set_limits(limits)?;
		}

		// Accept the connection.
		// NOTE: subscribe and publish seem backwards because of how relays work.
		// We publish the tracks the client is allowed to subscribe to.
		// We subscribe to the tracks the client is allowed to publish.
		//
		// moq-net defaults the unset side to a fresh no-op origin, which is fine for a
		// publish-only or subscribe-only session.
		let mut request = request
			.with_stats(stats.clone())
			.with_limits(lifecycle.cluster.limits(&lease));
		if let Some(subscriber) = subscriber {
			request = request.with_publisher(subscriber);
		}
		if let Some(publisher) = publisher {
			request = request.with_subscriber(publisher);
		}
		let lease = lease.preparing(request.auth(), request.role())?;
		let serving = lifecycle.shutdown.serve();
		// Bounded too: a peer that stalls the SETUP reply must not outlive the relay.
		let (session, driver) = lifecycle
			.shutdown
			.until_deadline(request.ok())
			.await
			.ok_or_else(|| anyhow::anyhow!("the SETUP reply was still pending at the drain deadline"))?
			.inspect_err(|err| setup_failed(&stats, err.downcast_ref()))?;

		// Joined rather than awaited in turn, for a driver that runs only when polled. A
		// spawned driver's panic still surfaces here, so it cannot look like cancellation.
		let (result, driven) = futures::join!(
			lifecycle.supervise(serving, session, lease, registration, transport),
			R::drive(driver),
		);
		driven?;
		result
	}
}

/// The span every line about one connection logs under, on whichever runtime emits it.
fn span(id: u64, remote: Option<std::net::SocketAddr>) -> tracing::Span {
	tracing::info_span!(
		"conn",
		id,
		remote = remote.map(tracing::field::display),
		session = tracing::field::Empty,
		implementation = tracing::field::Empty
	)
}

impl Lifecycle {
	pub(crate) fn new(
		cluster: cluster::Cluster,
		auth: auth::Auth,
		id: u64,
		remote: Option<std::net::SocketAddr>,
	) -> Self {
		Self {
			id,
			span: span(id, remote),
			cluster,
			auth,
			shutdown: crate::shutdown::Observer::disabled(),
			sessions: None,
		}
	}

	pub(crate) fn with_shutdown(mut self, shutdown: crate::shutdown::Observer) -> Self {
		self.shutdown = shutdown;
		self
	}

	pub(crate) fn with_sessions(mut self, sessions: crate::session::Registry) -> Self {
		self.sessions = Some(sessions);
		self
	}

	/// Run `task` in this connection's span, then log its close; a panic closes it as an error.
	pub(crate) async fn run(&self, task: impl std::future::Future<Output = anyhow::Result<()>>) {
		let result = self.isolate(task).await;
		self.closed(result);
	}

	/// Run the ring worker's half, then supervise the started session on `runtime`, which logs
	/// the close there: a worker stopped at shutdown drops its tasks, and the line with them.
	#[cfg(all(target_os = "linux", feature = "_uring"))]
	pub(crate) async fn start<S>(
		&self,
		runtime: &tokio::runtime::Handle,
		start: impl std::future::Future<Output = anyhow::Result<S>>,
	) where
		S: std::future::Future<Output = anyhow::Result<()>> + Send + 'static,
	{
		match self.isolate(start).await {
			Ok(supervise) => {
				let lifecycle = self.clone();
				runtime.spawn(async move { lifecycle.run(supervise).await });
			}
			Err(err) => self.closed(Err(err)),
		}
	}

	async fn isolate<T>(&self, task: impl std::future::Future<Output = anyhow::Result<T>>) -> anyhow::Result<T> {
		// Boxing keeps large protocol futures off nested debug-build stack frames.
		moq_tokio::panic::isolate(Box::pin(task))
			.instrument(self.span.clone())
			.await
			.unwrap_or_else(|| Err(anyhow::anyhow!("connection panicked")))
	}

	/// Logged here, under the span's own target, so every transport pairs its `conn{id=N}`
	/// with its close under one log filter.
	fn closed(&self, result: anyhow::Result<()>) {
		let _span = self.span.enter();
		match result {
			Ok(()) => tracing::info!(id = self.id, "connection closed"),
			Err(err) => tracing::warn!(id = self.id, %err, "connection closed"),
		}
	}

	/// Keep the connection span across a worker-to-shared-runtime handoff and return panics to its owner.
	/// Dropping the returned future aborts the task, so it never outlives its connection.
	#[cfg(all(target_os = "linux", feature = "_uring"))]
	pub(crate) async fn shared<T: Send + 'static>(
		&self,
		runtime: &tokio::runtime::Handle,
		task: impl std::future::Future<Output = T> + Send + 'static,
	) -> anyhow::Result<T> {
		use anyhow::Context as _;

		struct AbortOnDrop(tokio::task::AbortHandle);
		impl Drop for AbortOnDrop {
			fn drop(&mut self) {
				self.0.abort();
			}
		}

		let task = runtime.spawn(moq_tokio::panic::isolate(task).instrument(self.span.clone()));
		let _abort = AbortOnDrop(task.abort_handle());
		task.await
			.context("connection task failed")?
			.ok_or_else(|| anyhow::anyhow!("connection task panicked"))
	}

	pub(crate) async fn admit(
		&self,
		request: moq_auth::Request,
	) -> Result<(cluster::Admitted, Option<crate::session::Registration>), StatusError> {
		self.span.record("session", &request.id);
		// Peer-controlled, so recorded escaped rather than displayed raw.
		if let Some(implementation) = &request.implementation {
			self.span
				.record("implementation", tracing::field::debug(implementation));
		}
		let admitted = self.shutdown.admit(self.authorize(request)).await;
		// Cluster admission records its own refusals; LAN and drain refusals are recorded here.
		if let Err(err) = &admitted
			&& let Some(refusal) = err.refusal
		{
			self.cluster.refusals.record(refusal);
		}
		admitted
	}

	async fn authorize(
		&self,
		request: moq_auth::Request,
	) -> Result<(cluster::Admitted, Option<crate::session::Registration>), StatusError> {
		if cluster::Cluster::is_lan_path(&request.path) {
			let Some(presented) = cluster::Cluster::lan_credential(&request.path) else {
				return Err(StatusError {
					status: http::StatusCode::FORBIDDEN,
					refusal: Some(RefusalKind::Lan),
					source: anyhow::anyhow!("LAN peer did not present a membership proof"),
				});
			};
			match self.cluster.verify_lan_credential(presented) {
				Some(true) => {
					tracing::info!("accepted LAN peer");
					let lease = self.auth.admit_fixed("/", self.cluster.lan_peer_grant());
					return Ok((self.cluster.scope(lease, &request)?, None));
				}
				Some(false) => {
					return Err(StatusError {
						status: http::StatusCode::FORBIDDEN,
						refusal: Some(RefusalKind::Lan),
						source: anyhow::anyhow!("LAN peer did not present this listener's membership proof"),
					});
				}
				None => {
					return Err(StatusError {
						status: http::StatusCode::FORBIDDEN,
						refusal: Some(RefusalKind::Lan),
						source: anyhow::anyhow!("/.cluster request refused: LAN discovery is not enabled"),
					});
				}
			}
		}
		if request.tls.is_some() {
			tracing::debug!("client certificate verified; reported to the auth server");
		}
		let admitted = self
			.cluster
			.admit_session(&self.auth, request.clone())
			.await
			.map_err(|err| {
				// Cluster admission owns this refusal; only local/drain refusals remain here.
				let mut error = StatusError::from(err);
				error.refusal = None;
				error
			})?;
		let registration = self.sessions.as_ref().map(|sessions| sessions.register(request));
		Ok((admitted, registration))
	}

	pub(crate) async fn supervise(
		&self,
		serving: crate::shutdown::Serving,
		session: moq_net::Session,
		lease: auth::Lease,
		registration: Option<crate::session::Registration>,
		transport: impl std::fmt::Display,
	) -> anyhow::Result<()> {
		tracing::info!(version = %session.version(), %transport, "negotiated");
		supervise(session, lease, serving, registration).await
	}
}

/// Hold an accepted session open for as long as its lease allows.
///
/// Public so an embedder running its own accept loop (`moq --listen`) holds a
/// session the same way the relay does. Pass the [`session::Registration`](crate::session::Registration)
/// from [`Registry::register`](crate::session::Registry::register) so a push on
/// the internal listener re-checks this lease; `None` for a session that is
/// not in the table. Take `serving` with [`Observer::serve`](crate::shutdown::Observer::serve)
/// before the SETUP reply.
///
/// The lease is the decider's live word on the grant: a re-check re-authorizes the
/// session in place, narrower or wider, and when it stops covering the session
/// ([`auth::Lease::ended`]) the session closes with the reason. The session's own
/// close is reported back through the lease as the `end` event, and this returns only
/// once the decider heard it (bounded by the auth client's request timeout). Either
/// way, a relay shutdown drains the session with a GOAWAY instead of cutting it off,
/// and its drain waits for this to return, past its deadline if the cleanup needs it.
///
/// The session handle is `Send + Sync` whatever transport carries it, so this
/// runs on the shared runtime even for sessions a pinned QUIC worker drives.
pub async fn supervise(
	session: moq_net::Session,
	lease: auth::Lease,
	serving: crate::shutdown::Serving,
	registration: Option<crate::session::Registration>,
) -> anyhow::Result<()> {
	let mut lease = lease.authorizing(&session);
	let mut shutdown = serving.shutdown.clone();
	// Held until the `end` is reported, so a drain past its deadline still waits for it.
	let _serving = serving;
	let reported = lease.reported();
	let result = loop {
		let nudged = async {
			match &registration {
				Some(registration) => registration.nudged().await,
				None => std::future::pending().await,
			}
		};
		tokio::select! {
			err = session.closed() => {
				lease.close_session(&err, session_bytes(&session));
				break Err(err.into());
			}
			why = lease.ended() => {
				tracing::info!(%why, "lease ended, closing session");
				session.abort(crate::auth::error(&why));
				lease.close(why, session_bytes(&session));
				break Ok(());
			}
			_ = shutdown.started() => {
				tracing::info!("relay shutting down; draining session");
				// Empty URI: "reconnect to me" (the relay is restarting). The session's
				// machine runs on its own, so the GOAWAY still reaches the wire while
				// we wait here, and the drain deadline closes it if the peer stays.
				shutdown.drain_session(&session).await;
				lease.close(moq_auth::lease::Reason::Shutdown, session_bytes(&session));
				break Ok(());
			}
			() = nudged => lease.revalidate(),
		}
	};
	reported.await;
	result
}

/// The transport's own totals, read once at the end so the `end` event carries
/// what the session moved without the payload path paying for a second meter.
pub(crate) fn session_bytes(session: &moq_net::Session) -> moq_auth::Bytes {
	let stats = session.stats();
	moq_auth::Bytes {
		sent: stats.bytes_sent.unwrap_or_default(),
		received: stats.bytes_received.unwrap_or_default(),
	}
}

/// Admission already started the stats session, so a failed SETUP reply ends it.
/// A cause without a MoQ error is the transport's.
pub(crate) fn setup_failed(stats: &moq_net::stats::Session, cause: Option<&moq_net::Error>) {
	if let Some(kind) = cause.map_or(Some(moq_net::stats::Failure::Transport), failure) {
		stats.fail(kind);
	}
}

/// Close classification comes only from typed protocol and transport causes.
pub(crate) fn failure(error: &moq_net::Error) -> Option<moq_net::stats::Failure> {
	use moq_net::stats::Failure;
	use moq_net::{Error, SessionError};
	match error {
		Error::Cancel | Error::Closed | Error::GoingAway | Error::Session(SessionError::Cancel) => None,
		Error::IdleTimeout => Some(Failure::IdleTimeout),
		Error::Transport(_) => Some(Failure::Transport),
		_ => Some(Failure::Application),
	}
}

#[cfg(all(test, target_os = "linux", feature = "_uring"))]
#[path = "../tests/integration/logs.rs"]
mod logs;

#[cfg(test)]
mod tests {
	use super::*;
	use moq_net::{Error, SessionError, stats::Failure};

	#[test]
	fn a_failed_setup_reply_ends_the_admitted_session_by_its_cause() {
		let outcome = |cause: Option<&Error>| {
			let registry = moq_net::stats::Registry::new(Default::default());
			let stats = registry.tier(moq_net::stats::Tier::default()).session("root");
			setup_failed(&stats, cause);
			drop(stats);
			registry.snapshot().sessions()[0].1.sessions_failed
		};
		assert_eq!(outcome(Some(&Error::Cancel)), Default::default());
		assert_eq!(outcome(None).count(Failure::Transport), 1);
		assert_eq!(outcome(Some(&Error::IdleTimeout)).count(Failure::IdleTimeout), 1);
		assert_eq!(outcome(Some(&Error::Timeout)).count(Failure::Application), 1);
	}

	#[test]
	fn clean_closes_are_not_failures_and_typed_causes_keep_their_kind() {
		for error in [
			Error::Cancel,
			Error::Closed,
			Error::GoingAway,
			Error::Session(SessionError::Cancel),
		] {
			assert_eq!(failure(&error), None);
		}
		assert_eq!(failure(&Error::IdleTimeout), Some(Failure::IdleTimeout));
		assert_eq!(
			failure(&Error::Transport("idle timeout".into())),
			Some(Failure::Transport),
			"messages never choose the kind"
		);
		assert_eq!(
			failure(&Error::Session(SessionError::App(42))),
			Some(Failure::Application)
		);
	}

	/// A panic in the ring's supervision, after the worker handed it to the shared runtime, still
	/// logs the connection's close.
	#[cfg(all(target_os = "linux", feature = "_uring"))]
	#[tokio::test(start_paused = true)]
	async fn shared_supervision_panic_logs_the_connection_close() {
		use tracing_subscriber::layer::SubscriberExt;
		let logs = super::logs::Logs::default();
		let subscriber = tracing_subscriber::fmt()
			.with_ansi(false)
			.with_writer(logs.clone())
			.finish()
			.with(
				tracing_subscriber::filter::Targets::new().with_target("moq_relay::connection", tracing::Level::INFO),
			);
		let _logging = tracing::subscriber::set_default(subscriber);
		let cluster = cluster::Cluster::new(cluster::Options::default()).unwrap();
		let (auth, _admissions) = auth::Auth::embedded("test");
		let lifecycle = Lifecycle::new(cluster, auth, 42, None);
		lifecycle
			.start(&tokio::runtime::Handle::current(), async {
				Ok::<_, anyhow::Error>(async {
					panic!("injected supervision panic");
					#[expect(
						unreachable_code,
						reason = "the panic is the point; the `Ok` names the future's output type"
					)]
					Ok::<_, anyhow::Error>(())
				})
			})
			.await;
		logs.wait_for("connection closed").await;
		let text = logs.text();
		assert!(text.contains("conn{id=42}"), "{text}");
		assert!(
			text.contains("connection closed id=42 err=connection panicked"),
			"{text}"
		);
		assert_eq!(text.matches("connection closed").count(), 1, "{text}");
	}
}
