//! Graceful shutdown coordination: the first shutdown signal fires a broadcast
//! that every accepted session observes, draining it with a GOAWAY before the
//! process exits.
//!
//! The drain has one deadline, fixed when it starts. A session accepted after
//! that (a cached DNS resolve, a pool alias) is sent a GOAWAY at once, carrying
//! only the time left, so no session outlives the window the relay promised.

use std::time::{Duration, Instant};

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

/// A per-connection handle observing the relay-wide shutdown broadcast.
///
/// Cheap to clone; each accepted session waits on [`started`](Self::started)
/// and drains itself via [`drain_session`](Self::drain_session) when it fires.
#[derive(Clone)]
pub struct Observer {
	rx: watch::Receiver<Option<Instant>>,
	/// How long a drained session may keep running before it is force-closed.
	pub drain_timeout: Duration,
}

impl Observer {
	/// Create the trigger and its observer half.
	pub fn new(drain_timeout: Duration) -> (Trigger, Self) {
		let (tx, rx) = watch::channel(None);
		(Trigger { tx, drain_timeout }, Self { rx, drain_timeout })
	}

	/// A handle that never fires, for callers without shutdown coordination
	/// (tests, embedders that manage their own lifecycle).
	pub fn disabled() -> Self {
		let (tx, rx) = watch::channel(None);
		// Leak-free: dropping the sender doesn't resolve `started` (it waits for
		// a deadline, not for channel closure).
		drop(tx);
		Self {
			rx,
			drain_timeout: crate::DEFAULT_DRAIN_TIMEOUT,
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

	/// When the drain window ends, if [`Trigger::start`] has fired.
	pub(crate) fn deadline(&self) -> Option<Instant> {
		*self.rx.borrow()
	}

	/// Drain `session` with an empty-URI GOAWAY ("reconnect to me"), waiting for
	/// the peer to leave.
	///
	/// The driver force-closes with [`moq_net::Error::GoawayTimeout`] at the
	/// drain's deadline: [`drain_timeout`](Self::drain_timeout) after the trigger
	/// fired, or after this call if it has not. So this resolves either way, on
	/// every version. A peer too old for GOAWAY (moq-lite-03 and earlier) keeps
	/// being served until then and is closed the same way; it simply never learns
	/// why, which beats cutting it off with no warning and no grace.
	///
	/// With no time left (a zero [`drain_timeout`](Self::drain_timeout), or a
	/// session accepted after the deadline) the session is closed at once.
	pub async fn drain_session(&self, session: &moq_net::Session) {
		let remaining = match *self.rx.borrow() {
			Some(deadline) => deadline.saturating_duration_since(Instant::now()),
			None => self.drain_timeout,
		};

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

		session.closed().await;
	}
}
