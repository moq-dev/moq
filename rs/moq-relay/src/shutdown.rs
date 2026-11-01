//! Graceful shutdown coordination: the first shutdown signal fires a broadcast
//! that every accepted session observes, draining it with a GOAWAY before the
//! process exits.
//!
//! The drain has one deadline, fixed when it starts. A session accepted after
//! that (a cached DNS resolve, a pool alias) is sent a GOAWAY at once, carrying
//! only the time left, so no session outlives the window the relay promised.
//!
//! The drain ends as soon as every session has left. Accepted transports hold it
//! through handshake and authentication, then through the established session,
//! or until the shared deadline. The deadline cuts what is still pending and
//! closes every established session left, then waits for those to finish their
//! cleanup: the lease's `end`, reported to the decider.

use std::{sync::Arc, time::Duration};

use moq_clock::Instant;
use tokio::sync::watch;

/// Fires the relay-wide shutdown broadcast. Held by `Relay::run` for the OS
/// signal path; an embedder clones one to stop the relay from its own task.
#[derive(Clone)]
pub struct Trigger {
	tx: watch::Sender<Option<Instant>>,
	drain_timeout: Duration,
}

impl Trigger {
	/// Start the drain: every [`Observer`] handle's [`started`](Observer::started)
	/// resolves and sessions begin sending GOAWAY, including any accepted later.
	/// Calling it again keeps the first deadline.
	pub fn start(&self) {
		let deadline = Instant::now() + self.drain_timeout;
		self.tx.send_if_modified(|started| {
			let first = started.is_none();
			if first {
				*started = Some(deadline);
			}
			first
		});
	}
}

/// SIGINT or, on unix, SIGTERM (what process supervisors send on stop), caught
/// from construction on.
///
/// Building one replaces the default action, which kills the process, for the
/// rest of its life: tokio never uninstalls a handler, even once this drops. So
/// build it once nothing that can fail is left before serving, or a caller that
/// recovers from that failure is left ignoring its signals.
pub struct Signals {
	#[cfg(unix)]
	interrupt: tokio::signal::unix::Signal,
	#[cfg(unix)]
	terminate: tokio::signal::unix::Signal,
	#[cfg(not(unix))]
	ctrl_c: tokio::signal::windows::CtrlC,
	/// A signal [`pending`](Self::pending) took, still owed to [`recv`](Self::recv).
	queued: bool,
}

impl Signals {
	/// Install the handlers.
	pub fn new() -> anyhow::Result<Self> {
		use anyhow::Context;
		#[cfg(unix)]
		{
			use tokio::signal::unix::{SignalKind, signal};
			Ok(Self {
				interrupt: signal(SignalKind::interrupt()).context("failed to listen for SIGINT")?,
				terminate: signal(SignalKind::terminate()).context("failed to listen for SIGTERM")?,
				queued: false,
			})
		}
		#[cfg(not(unix))]
		{
			let ctrl_c = tokio::signal::windows::ctrl_c().context("failed to listen for Ctrl-C")?;
			Ok(Self { ctrl_c, queued: false })
		}
	}

	/// Whether a signal has arrived that [`recv`](Self::recv) has not returned
	/// yet, without waiting. One the runtime has not processed yet reads as none.
	pub fn pending(&mut self) -> bool {
		use futures::FutureExt;
		if !self.queued {
			self.queued = self.next().now_or_never().is_some();
		}
		self.queued
	}

	/// Resolve on the next signal, including one caught before this was polled.
	pub async fn recv(&mut self) {
		if !std::mem::take(&mut self.queued) {
			self.next().await;
		}
	}

	/// Cancel safe: a signal is only taken when this resolves.
	async fn next(&mut self) {
		#[cfg(unix)]
		tokio::select! {
			_ = self.interrupt.recv() => {}
			_ = self.terminate.recv() => {}
		}
		#[cfg(not(unix))]
		self.ctrl_c.recv().await;
	}
}

/// The sessions a drain is waiting on.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Tally {
	/// Established sessions still open or still reporting their end, drained or not.
	pub live: usize,
	/// Sessions sent a GOAWAY that have not left yet.
	pub draining: usize,
	/// Sessions still open when their drain deadline passed, and so closed by it.
	pub forced: usize,
}

/// A per-connection handle observing the relay-wide shutdown broadcast.
///
/// Cheap to clone; each accepted session waits on [`started`](Self::started)
/// and drains itself via [`drain_session`](Self::drain_session) when it fires.
#[derive(Clone)]
pub struct Observer {
	pub(crate) activity: moq_net::server::Activity,
	rx: watch::Receiver<Option<Instant>>,
	tally: Arc<watch::Sender<Tally>>,
	/// How long a drained session may keep running before it is force-closed.
	pub drain_timeout: Duration,
}

impl Observer {
	/// Create the trigger and its observer half.
	pub fn new(drain_timeout: Duration) -> (Trigger, Self) {
		let (tx, rx) = watch::channel(None);
		(Trigger { tx, drain_timeout }, Self::from(rx, drain_timeout))
	}

	/// A handle that never fires, for callers without shutdown coordination
	/// (tests, embedders that manage their own lifecycle).
	pub fn disabled() -> Self {
		let (tx, rx) = watch::channel(None);
		// Leak-free: dropping the sender doesn't resolve `started` (it waits for
		// a deadline, not for channel closure).
		drop(tx);
		Self::from(rx, crate::DEFAULT_DRAIN_TIMEOUT)
	}

	fn from(rx: watch::Receiver<Option<Instant>>, drain_timeout: Duration) -> Self {
		Self {
			rx,
			tally: Arc::new(watch::channel(Tally::default()).0),
			activity: Default::default(),
			drain_timeout,
		}
	}

	/// Resolve once the shutdown broadcast fires, at once if it already has.
	/// Never resolves for [`disabled`](Self::disabled) handles.
	pub async fn started(&mut self) {
		// wait_for returns Err once the sender is dropped without firing; park
		// forever in that case (a dropped trigger means no shutdown, not shutdown).
		if self.rx.wait_for(Option::is_some).await.is_err() {
			std::future::pending::<()>().await;
		}
	}

	/// Run `work` until the drain deadline, or `None` if the deadline passes first.
	/// The deadline wins a tie: work that finishes late, polled late, is still late.
	pub async fn until_deadline<T>(&self, work: impl Future<Output = T>) -> Option<T> {
		tokio::select! {
			biased;
			() = self.expired() => None,
			res = work => Some(res),
		}
	}

	/// Await an admission decision, refusing it as unavailable at the drain
	/// deadline so no pending decision outlives the relay.
	pub(crate) async fn admit<T, E: From<crate::auth::Error>>(
		&self,
		decision: impl Future<Output = Result<T, E>>,
	) -> Result<T, E> {
		self.until_deadline(decision).await.unwrap_or_else(|| {
			Err(crate::auth::Error::Unavailable("the admission was still pending at the drain deadline".into()).into())
		})
	}

	/// Resolve at the drain deadline, once the drain has started. Never resolves
	/// for [`disabled`](Self::disabled) handles.
	pub(crate) async fn expired(&self) {
		let mut rx = self.rx.clone();
		let deadline = match rx.wait_for(Option::is_some).await {
			Ok(deadline) => *deadline,
			Err(_) => None,
		};
		match deadline {
			Some(deadline) => moq_clock::sleep_until(deadline).await,
			None => std::future::pending().await,
		}
	}

	/// The admission tracker the drain waits on; give it to your own listener's `Config::activity`.
	pub fn activity(&self) -> &moq_net::server::Activity {
		&self.activity
	}

	/// When the drain window ends, if [`Trigger::start`] has fired.
	pub(crate) fn deadline(&self) -> Option<Instant> {
		*self.rx.borrow()
	}

	/// Count a session until the returned guard drops, so the drain waits for it to
	/// leave and report its end. Hand the guard to [`supervise`](crate::supervise).
	///
	/// Take it before the session's SETUP reply, while the connection's admission
	/// still holds the drain open: the transport driver may release that admission
	/// once the reply is out, and a drain that empties in between would not wait for
	/// the session. Bound the reply with [`until_deadline`](Self::until_deadline)
	/// too, or a peer stalling it holds the drain past its deadline.
	///
	/// One served after the deadline is closed at once and not counted: arrivals
	/// a listener still lets through must not hold a drain that is past it.
	pub fn serve(&self) -> Serving {
		let counted = self.deadline().is_none_or(|deadline| Instant::now() < deadline);
		if counted {
			self.tally.send_modify(|tally| tally.live += 1);
		}
		Serving {
			_active: self.activity.enter(),
			counted,
			shutdown: self.clone(),
		}
	}

	/// The sessions counted so far.
	pub(crate) fn tally(&self) -> Tally {
		*self.tally.borrow()
	}

	/// Resolve once the drain is over, logging how it ended: every admitted
	/// connection has left, refusing any that arrives after that, or the deadline
	/// passed and every established session it closed has reported its end.
	///
	/// Pending handshakes and admissions are cut at the deadline rather than waited
	/// for, so they never hold this past it. Waits for [`Trigger::start`] first, and
	/// never resolves for a [`disabled`](Self::disabled) handle.
	pub async fn drained(&self) {
		self.clone().started().await;
		tokio::select! {
			biased;
			() = self.activity.close() => {}
			() = self.expired() => {}
		}
		// Either way, wait for the counted sessions: past the deadline they are still
		// reporting their end, and before it one served without an admission still
		// holds the drain. Sessions served after the deadline are not counted, so
		// this only falls.
		let mut tally = self.tally.subscribe();
		// The sender lives in `self`, so this cannot close.
		let _ = tally.wait_for(|tally| tally.live == 0).await;

		let deadline = self.deadline().expect("started");
		let elapsed = Instant::now().saturating_duration_since(deadline - self.drain_timeout);
		match self.tally().forced {
			0 => tracing::info!(?elapsed, "drain ended with every session gone; exiting"),
			forced => tracing::warn!(?elapsed, forced, "drain deadline force-closed sessions; exiting"),
		}
	}

	/// Count a session as draining until the returned guard drops, and as forced
	/// if it is still open at `deadline`.
	fn draining(&self, deadline: Instant) -> Draining {
		self.tally.send_modify(|tally| tally.draining += 1);
		Draining {
			tally: self.tally.clone(),
			deadline,
		}
	}

	/// Drain `session` with an empty-URI GOAWAY ("reconnect to me"), waiting for
	/// the peer to leave.
	///
	/// This force-closes with [`moq_net::Error::GoawayTimeout`] at the drain's
	/// deadline: [`drain_timeout`](Self::drain_timeout) after the trigger fired, or
	/// after this call if it has not. It does not rely on the driver's own GOAWAY
	/// timer, which starts only once the message is on the wire, so a peer that
	/// stalls that write is still closed on time. A peer too old for GOAWAY
	/// (moq-lite-03 and earlier) keeps being served until then and is closed the same
	/// way; it simply never learns why, which beats cutting it off with no warning
	/// and no grace.
	///
	/// With no time left (a zero [`drain_timeout`](Self::drain_timeout), or a
	/// session accepted after the deadline) the session is closed at once.
	pub async fn drain_session(&self, session: &moq_net::Session) {
		let now = Instant::now();
		let deadline = self.deadline().unwrap_or(now + self.drain_timeout);
		let remaining = deadline.saturating_duration_since(now);
		// Dropped when this returns or is cancelled (a WebSocket driver ending
		// first), so the count holds whichever way the session goes.
		let _draining = self.draining(deadline);

		// No grace left, so there is nothing to drain. Close now rather than send a
		// GOAWAY: the peer would have no time to act on it, and a zero timeout means
		// "no deadline" on the wire, which is the opposite of what was asked for.
		if remaining.is_zero() {
			session.abort(moq_net::Error::GoingAway);
			return;
		}

		// "Reconnect to me": the relay is restarting, not moving. An empty URI is
		// legal from either side and this runs once per session, so neither error
		// is reachable; the deadline closes the session regardless.
		if let Err(err) = session
			.drain()
			.send(moq_net::goaway::Goaway::new().with_timeout(remaining))
		{
			tracing::warn!(%err, "failed to drain session");
		}

		tokio::select! {
			biased;
			_ = session.closed() => return,
			() = moq_clock::sleep_until(deadline) => {}
		}
		tracing::debug!("peer did not leave before the drain deadline; closing");
		session.abort(moq_net::Error::GoawayTimeout);
		session.closed().await;
	}
}

/// A session the drain waits on, from [`Observer::serve`].
#[must_use = "the drain stops waiting for the session once this drops"]
pub struct Serving {
	// `None` only once the drain closed admission, which the connection's own
	// admission prevents while it is held before the SETUP reply.
	_active: Option<moq_net::server::Active>,
	/// False for a session served after the deadline, which is not counted.
	counted: bool,
	pub(crate) shutdown: Observer,
}

impl Drop for Serving {
	fn drop(&mut self) {
		if self.counted {
			self.shutdown.tally.send_modify(|tally| tally.live -= 1);
		}
	}
}

/// A session sent a GOAWAY; see [`Observer::draining`].
struct Draining {
	tally: Arc<watch::Sender<Tally>>,
	deadline: Instant,
}

impl Drop for Draining {
	fn drop(&mut self) {
		// The force-close fires at `deadline`, so every session it closes is
		// counted. A peer leaving in that sliver was still there at the deadline too.
		let forced = Instant::now() >= self.deadline;
		self.tally.send_modify(|tally| {
			tally.draining -= 1;
			tally.forced += usize::from(forced);
		});
	}
}

/// Held by every test that raises a signal: it reaches each listener in the
/// process, and `cargo test` runs tests as threads of one.
#[cfg(all(test, unix))]
pub(crate) static SIGNALS: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// Raise `signal` at this process and wait until the runtime has processed it,
/// so every [`Signals`] built before this sees it.
#[cfg(all(test, unix))]
pub(crate) async fn raise(signal: libc::c_int) {
	let mut probe = Signals::new().unwrap();
	// SAFETY: raise has no memory-safety preconditions.
	assert_eq!(unsafe { libc::raise(signal) }, 0);
	probe.recv().await;
}

#[cfg(test)]
mod tests {
	use super::*;

	/// Checking for a signal keeps it for `recv`, so a caller that only looks
	/// before READY still drains on it.
	#[cfg(unix)]
	#[tokio::test]
	async fn a_pending_signal_is_still_received() {
		use futures::FutureExt;
		let _signals = SIGNALS.lock().await;
		let mut signals = Signals::new().unwrap();
		assert!(!signals.pending());
		raise(libc::SIGTERM).await;
		assert!(signals.pending());
		assert!(signals.pending(), "checking twice took the signal");
		assert!(signals.recv().now_or_never().is_some(), "the checked signal was lost");
		assert!(!signals.pending(), "one signal was received twice");
	}

	#[tokio::test]
	async fn pending_admission_holds_drain() {
		use futures::FutureExt;
		let mut config = crate::Config::default();
		config.listen.tcp.bind = Some("127.0.0.1:0".parse().unwrap());
		let mut relay = crate::Relay::load(config).await.unwrap().with_signals(false);
		let url: url::Url = format!("tcp://{}/room", relay.tcp_addr().unwrap()).parse().unwrap();
		let mut admissions = relay.admissions().unwrap();
		let trigger = relay.shutdown_trigger().clone();
		let shutdown = relay.shutdown().clone();
		let running = tokio::spawn(relay.run());
		let connecting = tokio::spawn(async move {
			moq_tokio::connect::Config::default()
				.init(Default::default())
				.unwrap()
				.with_reconnect(false)
				.connect(url)
				.established()
				.await
		});
		let admission = admissions.next().await.unwrap();
		trigger.start();
		let waited = shutdown.drained().now_or_never().is_none();
		admission.refuse(crate::auth::Error::Refused(Default::default()));
		drop(connecting.await.unwrap());
		running.await.unwrap().unwrap();
		assert!(waited, "drain completed while auth admission is pending");
	}

	/// A WebSocket peer that upgrades and never sends SETUP ends at the drain
	/// deadline even with the SETUP timeout disabled, so its task does not
	/// outlive the relay.
	#[cfg(all(feature = "quic", feature = "websocket"))]
	#[tokio::test]
	async fn withheld_websocket_setup_ends_at_the_deadline() {
		let window = Duration::from_secs(1);
		let mut config = crate::Config::default();
		config.listen.timeout = Some((Duration::ZERO).into());
		config.listen.bind = Some("127.0.0.1:0".parse().unwrap());
		config.listen.tls.generate = vec!["localhost".into()];
		config.web.ws = true;
		config.web.http.listen = Some("127.0.0.1:0".parse().unwrap());
		config.auth.public = vec![moq_auth::Pattern::all()];
		config.drain_timeout = Some((window).into());
		let relay = crate::Relay::load(config).await.unwrap().with_signals(false);
		let addr = relay.web_addrs().http.unwrap();
		let trigger = relay.shutdown_trigger().clone();
		let activity = relay.shutdown().activity.clone();
		let running = tokio::spawn(relay.run());

		// Upgraded, then silent: no MoQ SETUP ever follows.
		let _session = qmux::ws::Client::new()
			.with_protocols(moq_net::ALPNS.iter().map(|&a| (a, &[] as &[qmux::Version])))
			.connect(&format!("ws://{addr}/room"))
			.await
			.unwrap();

		// Only timers decide what follows.
		tokio::time::pause();
		trigger.start();
		moq_clock::timeout(window + Duration::from_secs(1), activity.close())
			.await
			.expect("a withheld SETUP outlived the drain deadline");
		running.await.unwrap().unwrap();
	}

	/// An embedder's own listener shares the relay's admission tracking through
	/// [`Observer::activity`], so a connection it holds keeps the drain open.
	#[tokio::test(start_paused = true)]
	async fn embedder_admission_holds_drain() {
		let mut config = crate::Config::default();
		config.listen.tcp.bind = Some("127.0.0.1:0".parse().unwrap());
		let mut relay = crate::Relay::load(config).await.unwrap().with_signals(false);
		let _admissions = relay.admissions().unwrap();
		let active = relay.shutdown().activity().enter().unwrap();
		let trigger = relay.shutdown_trigger().clone();
		let running = tokio::spawn(relay.run());
		trigger.start();
		moq_clock::sleep(Duration::from_millis(100)).await;
		assert!(!running.is_finished(), "drain ended under an embedder's admission");
		drop(active);
		running.await.unwrap().unwrap();
	}

	/// A decision ready only once the deadline has passed is refused, however
	/// the executor orders the two.
	#[tokio::test(start_paused = true)]
	async fn a_decision_after_the_deadline_is_refused() {
		let window = Duration::from_secs(1);
		let (trigger, shutdown) = Observer::new(window);
		trigger.start();
		tokio::time::advance(window).await;
		for _ in 0..32 {
			let decided = shutdown.admit(async { Ok::<_, crate::auth::Error>(()) }).await;
			assert!(
				matches!(decided, Err(crate::auth::Error::Unavailable(_))),
				"admitted after the deadline"
			);
		}
	}

	/// An auth decision still pending at the drain deadline is abandoned then, so
	/// the connection task does not outlive the relay.
	#[tokio::test]
	async fn pending_admission_is_abandoned_at_the_deadline() {
		let window = Duration::from_secs(1);
		let mut config = crate::Config::default();
		config.listen.tcp.bind = Some("127.0.0.1:0".parse().unwrap());
		config.drain_timeout = Some((window).into());
		let mut relay = crate::Relay::load(config).await.unwrap().with_signals(false);
		let url: url::Url = format!("tcp://{}/room", relay.tcp_addr().unwrap()).parse().unwrap();
		let mut admissions = relay.admissions().unwrap();
		let trigger = relay.shutdown_trigger().clone();
		let running = tokio::spawn(relay.run());
		let connecting = tokio::spawn(async move {
			moq_tokio::connect::Config::default()
				.init(Default::default())
				.unwrap()
				.with_reconnect(false)
				.connect(url)
				.established()
				.await
		});
		let mut admission = admissions.next().await.unwrap();

		// Only timers decide what follows; the embedded admission's own timeout
		// is well past the drain window.
		tokio::time::pause();
		trigger.start();
		moq_clock::timeout(window + Duration::from_secs(1), admission.abandoned())
			.await
			.expect("the admission outlived the drain deadline");
		running.await.unwrap().unwrap();
		// The client may already count the session established (SETUP precedes
		// auth); either way the refusal ends it.
		if let Ok(connection) = connecting.await.unwrap() {
			assert!(connection.closed().await.is_err(), "an abandoned admission is refused");
		}
	}

	#[tokio::test]
	async fn pending_admission_receives_goaway_after_drain_starts() {
		let mut config = crate::Config::default();
		config.listen.tcp.bind = Some("127.0.0.1:0".parse().unwrap());
		let mut relay = crate::Relay::load(config).await.unwrap().with_signals(false);
		let url: url::Url = format!("tcp://{}/room", relay.tcp_addr().unwrap()).parse().unwrap();
		let mut admissions = relay.admissions().unwrap();
		let trigger = relay.shutdown_trigger().clone();
		let running = tokio::spawn(relay.run());
		let connecting = tokio::spawn(async move {
			moq_tokio::connect::Config::default()
				.init(Default::default())
				.unwrap()
				.with_reconnect(false)
				.connect(url)
				.established()
				.await
		});
		let admission = admissions.next().await.unwrap();
		trigger.start();
		let all = moq_auth::Pattern::all;
		let (_lease, consumer) = moq_auth::lease::Producer::new(moq_auth::Grant::new(all(), all()));
		admission.grant(consumer);
		let connection = connecting.await.unwrap().unwrap();
		let draining = connection.draining().unwrap();
		let goaway = moq_net::kio::wait(|waiter| draining.poll(waiter)).await.unwrap();
		assert_eq!(goaway.uri(), "");
		connection.closed().await.unwrap();
		running.await.unwrap().unwrap();
	}

	#[tokio::test(start_paused = true)]
	async fn drained_waits_for_every_session() {
		let (trigger, shutdown) = Observer::new(Duration::from_secs(10));
		let first = shutdown.serve();
		let second = shutdown.serve();
		trigger.start();

		let drained = tokio::spawn({
			let shutdown = shutdown.clone();
			async move { shutdown.drained().await }
		});
		drop(first);
		tokio::task::yield_now().await;
		assert!(!drained.is_finished(), "drained with a session still open");

		drop(second);
		drained.await.expect("drained task");
		assert_eq!(shutdown.tally(), Tally::default());
	}

	/// A drain whose admissions emptied before the deadline still waits for a session
	/// counted without one, rather than racing its cleanup.
	#[tokio::test(start_paused = true)]
	async fn an_emptied_drain_still_waits_for_counted_sessions() {
		let (trigger, shutdown) = Observer::new(Duration::from_secs(10));
		trigger.start();
		shutdown.activity.close().await;
		let serving = shutdown.serve();
		let drained = tokio::spawn({
			let shutdown = shutdown.clone();
			async move { shutdown.drained().await }
		});
		tokio::task::yield_now().await;
		assert!(!drained.is_finished(), "drained under a counted session");

		drop(serving);
		drained.await.expect("drained task");
	}

	/// Past the deadline the drain waits for each established session to finish its
	/// cleanup, but not for one served after it, so arrivals cannot keep it open.
	#[tokio::test(start_paused = true)]
	async fn the_deadline_waits_only_for_sessions_served_before_it() {
		let window = Duration::from_secs(10);
		let (trigger, shutdown) = Observer::new(window);
		let established = shutdown.serve();
		trigger.start();
		let drained = tokio::spawn({
			let shutdown = shutdown.clone();
			async move { shutdown.drained().await }
		});

		moq_clock::sleep(window).await;
		let late = shutdown.serve();
		tokio::task::yield_now().await;
		assert!(!drained.is_finished(), "drained under a session still cleaning up");

		drop(established);
		moq_clock::timeout(Duration::from_secs(1), drained)
			.await
			.expect("a session served after the deadline held the drain")
			.unwrap();
		drop(late);
		assert_eq!(shutdown.tally().live, 0);
	}

	#[tokio::test(start_paused = true)]
	async fn a_session_open_at_the_deadline_is_forced() {
		let window = Duration::from_secs(10);
		let (trigger, shutdown) = Observer::new(window);
		trigger.start();
		let deadline = shutdown.deadline().expect("started");

		let left = shutdown.draining(deadline);
		let straggler = shutdown.draining(deadline);
		assert_eq!(shutdown.tally().draining, 2);

		drop(left);
		moq_clock::sleep(window).await;
		drop(straggler);

		let tally = shutdown.tally();
		assert_eq!(tally.draining, 0);
		assert_eq!(tally.forced, 1, "only the session open at the deadline is forced");
	}
}
