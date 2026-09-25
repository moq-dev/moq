//! Graceful shutdown coordination: the first shutdown signal fires a broadcast
//! that every accepted session observes, draining it with a GOAWAY before the
//! process exits.
//!
//! The drain has one deadline, fixed when it starts. A session accepted after
//! that (a cached DNS resolve, a pool alias) is sent a GOAWAY at once, carrying
//! only the time left, so no session outlives the window the relay promised.
//!
//! The drain ends as soon as every session has left, or at that deadline.
//! Sessions are counted once established, so one still in
//! its handshake when the last established session leaves is cut off with the
//! process.

use std::{sync::Arc, time::Duration};

use tokio::{sync::watch, time::Instant};

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

/// The sessions a drain is waiting on.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Tally {
	/// Established sessions still open, drained or not.
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

	/// When the drain window ends, if [`Trigger::start`] has fired.
	pub(crate) fn deadline(&self) -> Option<Instant> {
		*self.rx.borrow()
	}

	/// Count an established session until the returned guard drops, so the drain
	/// waits for it to leave.
	pub(crate) fn serve(&self) -> Serving {
		self.tally.send_modify(|tally| tally.live += 1);
		Serving {
			tally: self.tally.clone(),
		}
	}

	/// The sessions counted so far.
	pub(crate) fn tally(&self) -> Tally {
		*self.tally.borrow()
	}

	/// Resolve once no [`serve`](Self::serve) guard is left.
	pub(crate) async fn drained(&self) {
		// The sender lives in `self`, so the channel cannot close under the wait.
		let _ = self.tally.subscribe().wait_for(|tally| tally.live == 0).await;
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

		session.closed().await;
	}
}

/// An established session the drain waits on; see [`Observer::serve`].
pub(crate) struct Serving {
	tally: Arc<watch::Sender<Tally>>,
}

impl Drop for Serving {
	fn drop(&mut self) {
		self.tally.send_modify(|tally| tally.live -= 1);
	}
}

/// A session sent a GOAWAY; see [`Observer::draining`].
struct Draining {
	tally: Arc<watch::Sender<Tally>>,
	deadline: Instant,
}

impl Drop for Draining {
	fn drop(&mut self) {
		// The driver arms its force-close after sending the GOAWAY, so it never
		// fires before `deadline` and every session it closes is counted. A peer
		// leaving in that sliver was still there at the deadline too.
		let forced = Instant::now() >= self.deadline;
		self.tally.send_modify(|tally| {
			tally.draining -= 1;
			tally.forced += usize::from(forced);
		});
	}
}

#[cfg(test)]
mod tests {
	use super::*;

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
		tokio::time::sleep(window).await;
		drop(straggler);

		let tally = shutdown.tally();
		assert_eq!(tally.draining, 0);
		assert_eq!(tally.forced, 1, "only the session open at the deadline is forced");
	}
}
