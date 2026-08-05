//! GOAWAY, the graceful drain signal, split into a [Producer] and [Consumer] handle.
//!
//! A session sends its own GOAWAY through the [Producer] from
//! [`Session::drain`](crate::Session::drain), and observes the peer's on the
//! [Consumer] from [`Session::draining`](crate::Session::draining). A session
//! sends at most one, which is why [`Producer::send`] consumes the handle.
//!
//! Draining works the same on a version with no GOAWAY message: the deadline is
//! the sender's own timer, so it force-closes on schedule whether or not the peer
//! could be told why. Callers do not branch on the negotiated version.
//!
//! Following the redirect is the caller's job. `moq_native::Reconnect` implements
//! it for native clients.

use std::{
	sync::{
		Arc,
		atomic::{AtomicBool, Ordering},
	},
	task::Poll,
	time::Duration,
};

use crate::{Error, Result, SessionError};

/// Maximum New Session URI length, in bytes. Both wires cap it here, and a
/// receiver treats anything longer as a protocol violation.
pub(crate) const MAX_URI: usize = 8192;

/// A GOAWAY: the sender intends to close the session soon and the peer should
/// migrate its subscriptions elsewhere.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct Goaway {
	/// Where the peer should reconnect, including any credentials it needs.
	/// Empty means reconnect to the same endpoint.
	pub uri: String,

	/// How long the sender waits before force-closing with
	/// [`Error::GoawayTimeout`]. `None` means no deadline.
	///
	/// Only moq-transport draft-17+ carries this on the wire. Elsewhere it still
	/// applies locally, so the sender enforces a deadline the peer cannot see.
	pub timeout: Option<Duration>,
}

impl Goaway {
	/// Tell the peer to reconnect to the same endpoint.
	pub fn new() -> Self {
		Self::default()
	}

	/// Tell the peer to reconnect to `uri`.
	///
	/// The URI must carry whatever credentials the peer needs: it is dialed as
	/// given, and nothing from the current session is copied onto it.
	pub fn redirect(uri: impl Into<String>) -> Self {
		Self {
			uri: uri.into(),
			timeout: None,
		}
	}

	/// Force-close the session with [`Error::GoawayTimeout`] if the peer is still
	/// around after `timeout`.
	pub fn with_timeout(mut self, timeout: Duration) -> Self {
		// The wire encodes 0 as "no deadline", so keep the local timer consistent
		// rather than force-closing almost immediately.
		self.timeout = (!timeout.is_zero()).then_some(timeout);
		self
	}
}

/// Sends this session's single GOAWAY.
///
/// Obtained from [`Session::drain`](crate::Session::drain).
pub struct Producer {
	trigger: kio::Producer<Option<Goaway>>,
	/// Whether this side may name a redirect URI. A moq-transport client may not,
	/// since it cannot tell a server to open connections (draft-19 sect 10.4).
	redirect: bool,
}

impl Producer {
	/// Send the GOAWAY, consuming the handle: a session may only send one.
	///
	/// The session must stay driven for the message to reach the wire. With
	/// [`Goaway::timeout`] set, the driver force-closes the session once the
	/// deadline passes, so awaiting [`Session::closed`](crate::Session::closed)
	/// is how a caller observes the drain finishing either way. That holds on a
	/// version with no GOAWAY message too, where the deadline is all the peer gets.
	///
	/// # Errors
	///
	/// Returns [`Error::ProtocolViolation`] when a moq-transport client names a
	/// redirect URI, which the peer would be required to close the session over,
	/// or when the URI exceeds the 8,192-byte wire cap.
	///
	/// Returns [`Error::Duplicate`] if this session already sent one. The wire
	/// carries at most one either way, so the second message is dropped rather
	/// than replacing the first, but the caller is told rather than left guessing.
	pub fn send(self, goaway: Goaway) -> Result<()> {
		if !self.redirect && !goaway.uri.is_empty() {
			return Err(Error::ProtocolViolation);
		}
		// Both wires cap the URI, so sending a longer one would just get the session
		// closed by the peer. Refuse it here, the one chokepoint both wires funnel
		// through, rather than at each encoder.
		if goaway.uri.len() > MAX_URI {
			return Err(Error::ProtocolViolation);
		}
		// The session closing first is not an error: there is nothing left to drain.
		if let Ok(mut state) = self.trigger.write() {
			if state.is_some() {
				return Err(Error::Duplicate);
			}
			*state = Some(goaway);
		}
		Ok(())
	}
}

/// Observes the peer's GOAWAY.
///
/// Obtained from [`Session::draining`](crate::Session::draining). Cheap to
/// clone; every clone reports the same GOAWAY.
#[derive(Clone)]
pub struct Consumer {
	state: kio::Consumer<Option<Goaway>>,
}

impl Consumer {
	/// The peer's GOAWAY, or `None` if it has not sent one.
	///
	/// The synchronous read behind [`recv`](Self::recv), for callers that only
	/// want to know whether the session is draining.
	pub fn peek(&self) -> Option<Goaway> {
		self.state.read().clone()
	}

	/// Poll for the peer's GOAWAY.
	///
	/// `Ready(Err)` once the session closes without one, so a caller can stop
	/// watching rather than parking forever.
	pub fn poll(&self, waiter: &kio::Waiter) -> Poll<Result<Goaway>> {
		match self.state.poll(waiter, |state| match &**state {
			Some(goaway) => Poll::Ready(goaway.clone()),
			None => Poll::Pending,
		}) {
			Poll::Ready(Ok(goaway)) => Poll::Ready(Ok(goaway)),
			Poll::Ready(Err(_)) => Poll::Ready(Err(Error::Closed)),
			Poll::Pending => Poll::Pending,
		}
	}

	/// Wait for the peer's GOAWAY, or `None` if the session closes without one.
	pub async fn recv(&self) -> Option<Goaway> {
		kio::wait(|waiter| self.poll(waiter)).await.ok()
	}
}

/// The receive-side GOAWAY signal handed to a subscriber.
///
/// Carries the flag and the channel together because a subscriber needs both: an
/// atomic load before opening a request stream (the hot path, which is why the
/// flag is not merely a read of the channel), and a wakeup for the per-source
/// tasks, which have to react to a peer that drains and then goes quiet.
#[derive(Clone)]
pub(crate) struct GoingAway {
	flag: Arc<AtomicBool>,
	received: Consumer,
}

impl GoingAway {
	/// Mark the session as going away, returning whether this was the first time.
	pub fn set(&self) -> bool {
		!self.flag.swap(true, Ordering::AcqRel)
	}

	/// Whether a GOAWAY has been received.
	pub fn is_set(&self) -> bool {
		self.flag.load(Ordering::Acquire)
	}

	/// Poll for the peer's GOAWAY, so a loop already polling other sources can
	/// react to a draining peer instead of waiting on a message it may never send.
	///
	/// Stays `Ready` once set, so a caller must treat what it does in response as
	/// idempotent rather than as an edge.
	pub fn poll(&self, waiter: &kio::Waiter) -> Poll<()> {
		// Presence only, rather than [`Consumer::poll`]: this stays ready for the rest
		// of the session, and every wakeup of every per-source task would otherwise
		// clone the URI just to throw it away.
		match self.received.state.poll(waiter, |state| match &**state {
			Some(_) => Poll::Ready(()),
			None => Poll::Pending,
		}) {
			Poll::Ready(Ok(())) => Poll::Ready(()),
			// The session closed without a GOAWAY. Nothing is draining, and the
			// caller's own loop is about to end on the same close.
			Poll::Ready(Err(_)) => Poll::Pending,
			Poll::Pending => Poll::Pending,
		}
	}
}

/// A detached signal for tests that build a subscriber without a session. Its
/// channel is already closed, so it never reports a GOAWAY.
#[cfg(test)]
impl Default for GoingAway {
	fn default() -> Self {
		let received = kio::Producer::new(None);
		Self {
			flag: Default::default(),
			received: Consumer {
				state: received.consume(),
			},
		}
	}
}

/// The halves handed to a protocol driver's `start()`.
#[derive(Clone)]
pub(crate) struct Protocol {
	/// Awaited by the driver's send path; firing means "encode and send GOAWAY now".
	trigger: kio::Consumer<Option<Goaway>>,
	/// Written by the driver's receive path when a GOAWAY is decoded.
	received: kio::Producer<Option<Goaway>>,
	/// Set alongside `received`; checked before opening new request streams.
	pub going_away: GoingAway,
}

impl Protocol {
	/// Record a decoded GOAWAY.
	///
	/// # Errors
	///
	/// Returns [`Error::ProtocolViolation`] on a second GOAWAY, which both drafts
	/// require the session be closed over (draft-19 sect 10.4). Keeping the first
	/// payload instead would leave an observer acting on a URI the peer has since
	/// replaced, with no way to tell.
	pub fn record(&self, goaway: Goaway) -> Result<()> {
		if !self.going_away.set() {
			return Err(Error::ProtocolViolation);
		}
		if let Ok(mut state) = self.received.write() {
			*state = Some(goaway);
		}
		Ok(())
	}

	/// Wait for the send trigger to fire, returning the GOAWAY to encode.
	///
	/// Returns `None` if the trigger was dropped without firing (the session is
	/// closing without a drain), so nothing should be sent.
	pub async fn triggered(&self) -> Option<Goaway> {
		kio::wait(|waiter| {
			match self.trigger.poll(waiter, |state| match &**state {
				Some(goaway) => Poll::Ready(goaway.clone()),
				None => Poll::Pending,
			}) {
				Poll::Ready(Ok(goaway)) => Poll::Ready(Some(goaway)),
				Poll::Ready(Err(_)) => Poll::Ready(None),
				Poll::Pending => Poll::Pending,
			}
		})
		.await
	}
}

/// Enforce a sent GOAWAY's deadline: close the session once it passes.
///
/// The draft makes the deadline the sender's promise, not the peer's, so the
/// driver arms this after the message hits the wire rather than making the
/// caller hold a handle and await it.
pub(crate) async fn enforce<S: web_transport_trait::Session>(session: &S, timeout: Option<Duration>) {
	let Some(timeout) = timeout else {
		return;
	};

	let mut closed = std::pin::pin!(session.closed());
	let mut deadline = std::pin::pin!(web_async::time::sleep(timeout));

	let expired = kio::wait(|waiter| {
		if waiter.poll_future(closed.as_mut()).is_ready() {
			return Poll::Ready(false);
		}
		waiter.poll_future(deadline.as_mut()).map(|_| true)
	})
	.await;

	if expired {
		tracing::warn!(?timeout, "peer did not leave before the GOAWAY deadline; closing");
		session.close(SessionError::GoawayTimeout.to_code(), &Error::GoawayTimeout.to_string());
	}
}

/// The halves held by the public [`crate::Session`].
pub(crate) struct Handle {
	/// Cloned into each [`Producer`] handed out by [`crate::Session::drain`].
	trigger: kio::Producer<Option<Goaway>>,
	/// Whether this side may name a redirect URI in its own GOAWAY.
	redirect: bool,
	/// Handed out by [`crate::Session::draining`].
	consumer: Consumer,
}

impl Handle {
	/// Create the session-side handle and its protocol-side counterpart.
	///
	/// `redirect` is whether this side may name a URI in its own GOAWAY.
	pub fn new(redirect: bool) -> (Self, Protocol) {
		let trigger = kio::Producer::new(None);
		let received = kio::Producer::new(None);
		let consumer = Consumer {
			state: received.consume(),
		};
		let going_away = GoingAway {
			flag: Default::default(),
			received: consumer.clone(),
		};

		let handle = Self {
			trigger: trigger.clone(),
			redirect,
			consumer,
		};
		let protocol = Protocol {
			trigger: trigger.consume(),
			received,
			going_away,
		};

		(handle, protocol)
	}

	/// A send half. Handing one out is always allowed; sending twice is what
	/// [`Producer::send`] refuses, so a caller never has to ask whether the slot
	/// is still free before deciding how to shut down.
	pub fn producer(&self) -> Producer {
		Producer {
			trigger: self.trigger.clone(),
			redirect: self.redirect,
		}
	}

	/// A fresh handle on the receive half.
	pub fn consumer(&self) -> Consumer {
		self.consumer.clone()
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	/// A session sends at most one GOAWAY. Handing out the handle is always
	/// allowed, so a caller never has to ask whether the slot is free; sending
	/// twice is what gets refused, and the first payload is the one kept.
	#[test]
	fn only_the_first_send_wins() {
		let (handle, protocol) = Handle::new(true);

		handle
			.producer()
			.send(Goaway::redirect("https://first.example/"))
			.unwrap();

		let err = handle
			.producer()
			.send(Goaway::redirect("https://second.example/"))
			.unwrap_err();
		assert!(matches!(err, Error::Duplicate));

		assert_eq!(
			protocol.trigger.read().as_ref().map(|g| g.uri.as_str()),
			Some("https://first.example/"),
			"the peer must not see a URI replaced out from under it"
		);
	}

	/// A moq-transport client cannot tell a server where to reconnect, so naming a
	/// URI is refused locally rather than getting the session closed by the peer.
	#[test]
	fn client_redirect_is_refused() {
		let (handle, _protocol) = Handle::new(false);
		let err = handle
			.producer()
			.send(Goaway {
				uri: "https://elsewhere.example/".to_string(),
				..Default::default()
			})
			.unwrap_err();
		assert!(matches!(err, Error::ProtocolViolation));

		// An empty URI ("I am going away") is still allowed.
		let (handle, _protocol) = Handle::new(false);
		handle.producer().send(Goaway::default()).unwrap();
	}

	/// A URI past the wire cap is refused at the send chokepoint rather than
	/// reaching an encoder that would frame it wrong.
	#[test]
	fn oversized_uri_is_refused() {
		let (handle, _protocol) = Handle::new(true);
		let err = handle
			.producer()
			.send(Goaway::redirect("x".repeat(MAX_URI + 1)))
			.unwrap_err();
		assert!(matches!(err, Error::ProtocolViolation));

		let (handle, _protocol) = Handle::new(true);
		handle
			.producer()
			.send(Goaway::redirect("x".repeat(MAX_URI)))
			.expect("exactly at the cap is fine");
	}

	/// A second GOAWAY is a protocol violation, not something to log and discard.
	#[test]
	fn duplicate_is_a_protocol_violation() {
		let (_handle, protocol) = Handle::new(true);
		protocol.record(Goaway::default()).unwrap();
		let err = protocol.record(Goaway::default()).unwrap_err();
		assert!(matches!(err, Error::ProtocolViolation));
	}

	/// The consumer reports the recorded GOAWAY both synchronously and by polling.
	#[tokio::test]
	async fn consumer_observes_the_recorded_goaway() {
		let (handle, protocol) = Handle::new(true);
		let consumer = handle.consumer();
		assert_eq!(consumer.peek(), None);

		let goaway = Goaway {
			uri: "https://elsewhere.example/".to_string(),
			timeout: Some(Duration::from_secs(5)),
		};
		protocol.record(goaway.clone()).unwrap();

		assert_eq!(consumer.peek(), Some(goaway.clone()));
		assert_eq!(consumer.recv().await, Some(goaway));
	}

	/// A session that closes without a GOAWAY resolves `recv` rather than parking
	/// forever, so a caller watching for one can stop.
	#[tokio::test]
	async fn recv_resolves_when_the_session_closes() {
		let (handle, protocol) = Handle::new(true);
		let consumer = handle.consumer();
		drop(protocol);
		assert_eq!(consumer.recv().await, None);
	}
}
