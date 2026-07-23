//! Graceful shutdown coordination: the first shutdown signal fires a broadcast
//! that every accepted session observes, draining it with a GOAWAY before the
//! process exits.

use std::time::Duration;

use tokio::sync::watch;

/// Fires the relay-wide shutdown broadcast. Held by `main`.
pub struct ShutdownTrigger {
	tx: watch::Sender<bool>,
}

impl ShutdownTrigger {
	/// Start the drain: every [`Shutdown`] handle's [`started`](Shutdown::started)
	/// resolves and sessions begin sending GOAWAY.
	pub fn start(&self) {
		let _ = self.tx.send(true);
	}
}

/// A per-connection handle observing the relay-wide shutdown broadcast.
///
/// Cheap to clone; each accepted session waits on [`started`](Self::started)
/// and drains itself via [`drain_session`](Self::drain_session) when it fires.
#[derive(Clone)]
pub struct Shutdown {
	rx: watch::Receiver<bool>,
	/// How long a drained session may keep running before it is force-closed.
	pub drain_timeout: Duration,
}

impl Shutdown {
	/// Create the trigger and its observer half.
	pub fn new(drain_timeout: Duration) -> (ShutdownTrigger, Self) {
		let (tx, rx) = watch::channel(false);
		(ShutdownTrigger { tx }, Self { rx, drain_timeout })
	}

	/// A handle that never fires, for callers without shutdown coordination
	/// (tests, embedders that manage their own lifecycle).
	pub fn disabled() -> Self {
		let (tx, rx) = watch::channel(false);
		// Leak-free: dropping the sender doesn't resolve `started` (it waits for
		// a `true` value, not for channel closure).
		drop(tx);
		Self {
			rx,
			drain_timeout: Duration::from_secs(crate::DEFAULT_DRAIN_TIMEOUT_SECS),
		}
	}

	/// Resolve once the shutdown broadcast fires. Never resolves for
	/// [`disabled`](Self::disabled) handles.
	pub async fn started(&mut self) {
		// wait_for returns Err once the sender is dropped without firing; park
		// forever in that case (a dropped trigger means no shutdown, not shutdown).
		if self.rx.wait_for(|started| *started).await.is_err() {
			std::future::pending::<()>().await;
		}
	}

	/// Drain `session` with an empty-URI GOAWAY ("reconnect to me"), waiting for
	/// the peer to leave and force-closing after [`Self::drain_timeout`].
	///
	/// Sessions on versions without GOAWAY (moq-lite-03 and earlier) are closed
	/// immediately with [`moq_net::Error::GoingAway`]; there is no wire message
	/// to warn them with.
	pub async fn drain_session(&self, session: &moq_net::Session) {
		match session.drain() {
			Some(drain) => drain.start_with_timeout("", self.drain_timeout).complete().await,
			None => session.abort(moq_net::Error::GoingAway),
		}
	}
}
