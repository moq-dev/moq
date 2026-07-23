//! GOAWAY signal plumbing shared between the public [`crate::Session`] API and the
//! protocol drivers (lite and IETF).
//!
//! The public types ([`GoawayReceived`]) are re-exported at the crate root; the
//! channel halves are crate-internal wiring created per session by `Client`/`Server`.

use std::{
	sync::{
		Arc,
		atomic::{AtomicBool, Ordering},
	},
	task::Poll,
	time::Duration,
};

/// Information from a received GOAWAY message.
///
/// The peer is telling us to reconnect to a new session at the provided URI
/// (or reconnect to the same endpoint if the URI is empty).
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct GoawayReceived {
	/// The URI to reconnect to. Empty means reconnect to the same endpoint.
	pub uri: Arc<str>,
	/// How long before the sender force-closes the session. `None` if not provided
	/// (moq-lite has no timeout on the wire; IETF draft-14-16 have no timeout field).
	pub timeout: Option<Duration>,
}

/// Payload carried by the send-side GOAWAY trigger from [`crate::Drain`] to the
/// protocol driver, which encodes it on whichever channel the version uses.
#[derive(Clone, Debug)]
pub(crate) struct Payload {
	/// Redirect URI (empty = reconnect to the same endpoint).
	pub uri: Arc<str>,
	/// Timeout before force-close. Encoded on versions with a wire timeout
	/// (IETF draft-17+); local-only elsewhere. `None` means no deadline.
	pub timeout: Option<Duration>,
}

/// A shared boolean set once a GOAWAY is received from the peer.
///
/// Checked by subscribers before opening new request streams, so gating is a
/// cheap load rather than a channel poll.
#[derive(Clone, Default)]
pub(crate) struct GoingAway(Arc<AtomicBool>);

impl GoingAway {
	/// Mark the session as going away. Returns whether this was the first set,
	/// so a duplicate GOAWAY can be detected and ignored (kept-first semantics).
	pub fn set(&self) -> bool {
		!self.0.swap(true, Ordering::AcqRel)
	}

	/// Whether a GOAWAY has been received.
	pub fn is_set(&self) -> bool {
		self.0.load(Ordering::Acquire)
	}
}

/// The halves handed to a protocol driver's `start()`.
#[derive(Clone)]
pub(crate) struct Protocol {
	/// Awaited by the driver's send path; firing means "encode and send GOAWAY now".
	pub trigger: kio::Consumer<Option<Payload>>,
	/// Written by the driver's receive path when a GOAWAY is decoded.
	pub received: kio::Producer<Option<GoawayReceived>>,
	/// Set alongside `received`; checked before opening new request streams.
	pub going_away: GoingAway,
}

impl Protocol {
	/// Record a decoded GOAWAY, ignoring duplicates (an observer may already be
	/// acting on the first payload, so a second must not swap it out).
	///
	/// Returns `false` for a duplicate, which the caller should log and discard.
	pub fn record(&self, received: GoawayReceived) -> bool {
		if !self.going_away.set() {
			return false;
		}
		if let Ok(mut state) = self.received.write() {
			*state = Some(received);
		}
		true
	}

	/// Wait for the send trigger to fire, returning the payload to encode.
	///
	/// Returns `None` if the trigger was dropped without firing (the session is
	/// closing without a drain), so no GOAWAY should be sent.
	pub async fn triggered(&self) -> Option<Payload> {
		kio::wait(|waiter| {
			match self.trigger.poll(waiter, |state| match &**state {
				Some(v) => Poll::Ready(v.clone()),
				None => Poll::Pending,
			}) {
				Poll::Ready(Ok(v)) => Poll::Ready(Some(v)),
				Poll::Ready(Err(_)) => Poll::Ready(None),
				Poll::Pending => Poll::Pending,
			}
		})
		.await
	}
}

/// The halves held by the public [`crate::Session`].
pub(crate) struct Handle {
	/// Fires the protocol driver's send path.
	pub trigger: kio::Producer<Option<Payload>>,
	/// Resolved by [`crate::Session::goaway`] when the peer sends a GOAWAY.
	pub received: kio::Consumer<Option<GoawayReceived>>,
	/// Mirror of the protocol-side flag for [`crate::Session::is_going_away`].
	pub going_away: GoingAway,
	/// Drain claim: only one GOAWAY per session.
	pub draining: Arc<AtomicBool>,
}

impl Handle {
	/// Create the session-side handle and its protocol-side counterpart.
	pub fn new() -> (Self, Protocol) {
		let trigger = kio::Producer::new(None);
		let received = kio::Producer::new(None);
		let going_away = GoingAway::default();
		let handle = Self {
			received: received.consume(),
			trigger: trigger.clone(),
			going_away: going_away.clone(),
			draining: Arc::new(AtomicBool::new(false)),
		};
		let protocol = Protocol {
			trigger: trigger.consume(),
			received,
			going_away,
		};
		(handle, protocol)
	}
}
