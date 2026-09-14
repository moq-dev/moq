//! The handle a session holds for the grant that admitted it.
//!
//! Whoever runs the accept loop decides how a session is authorized, builds a
//! [`Producer`], and hands the [`Consumer`] to the session. The session reads the
//! current [`Grant`], waits for it to change, and learns when it is revoked. The
//! producer side is driven by [`Client`](crate::Client) when an auth server answers, or
//! by any in-process logic when the embedder decides itself. No trait, no callbacks.

use std::task::Poll;

use serde::{Deserialize, Serialize};

use crate::Grant;

/// Why a lease ended, from whichever side ended it.
///
/// On the wire an `end` event carries it as one string: the fixed spellings below,
/// or the session's own classification verbatim.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Reason {
	/// A handle was dropped without saying why.
	Dropped,
	/// The grant reached its `expires`.
	Expired,
	/// The auth server refused the session on a re-check.
	Refused,
	/// The session ended for its own reason, named by whoever closed it.
	Session(String),
}

impl Reason {
	/// The wire spelling.
	pub fn as_str(&self) -> &str {
		match self {
			Self::Dropped => "dropped",
			Self::Expired => "expired",
			Self::Refused => "refused",
			Self::Session(reason) => reason,
		}
	}
}

impl std::fmt::Display for Reason {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		f.write_str(self.as_str())
	}
}

impl From<&str> for Reason {
	fn from(reason: &str) -> Self {
		match reason {
			"dropped" => Self::Dropped,
			"expired" => Self::Expired,
			"refused" => Self::Refused,
			other => Self::Session(other.to_string()),
		}
	}
}

impl From<String> for Reason {
	fn from(reason: String) -> Self {
		Self::from(reason.as_str())
	}
}

impl Serialize for Reason {
	fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
		serializer.serialize_str(self.as_str())
	}
}

impl<'de> Deserialize<'de> for Reason {
	fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
		Ok(String::deserialize(deserializer)?.into())
	}
}

#[derive(Debug)]
struct State {
	grant: Grant,
	/// Bumped on every update, so a consumer can tell a change from a spurious wake.
	epoch: u64,
	/// Set once by whichever side ends the lease first; the other side reads it.
	closed: Option<Reason>,
}

/// The authorizing side of a lease: applies new grants and revokes.
///
/// Dropping it revokes with [`Reason::Dropped`] unless the lease already ended.
#[derive(Debug)]
pub struct Producer {
	state: kio::Shared<State>,
}

impl Producer {
	/// Start a lease on `grant`, returning both handles.
	pub fn new(grant: Grant) -> (Self, Consumer) {
		let state = kio::Shared::new(State {
			grant,
			epoch: 0,
			closed: None,
		});
		(Self { state: state.clone() }, Consumer { state, seen: 0 })
	}

	/// Replace the grant, waking the consumer. A no-op once the lease ended.
	pub fn update(&self, grant: Grant) {
		let mut state = self.state.lock();
		if state.closed.is_some() {
			return;
		}
		state.grant = grant;
		state.epoch += 1;
	}

	/// End the lease with `reason`, consuming the handle. The consumer's
	/// [`closed`](Consumer::closed) resolves with it.
	pub fn revoke(self, reason: Reason) {
		self.close(reason);
	}

	/// Poll for the lease ending, from either side.
	pub fn poll_closed(&self, waiter: &kio::Waiter) -> Poll<Reason> {
		let state = std::task::ready!(self.state.poll(waiter, |state| ready_if(state.closed.is_some())));
		Poll::Ready(state.closed.clone().expect("waited for a close"))
	}

	/// Wait for the lease to end, from either side.
	pub async fn closed(&self) -> Reason {
		kio::wait(|waiter| self.poll_closed(waiter)).await
	}

	fn close(&self, reason: Reason) {
		self.state.lock().closed.get_or_insert(reason);
	}
}

impl Drop for Producer {
	fn drop(&mut self) {
		self.close(Reason::Dropped);
	}
}

/// The session's side of a lease: reads the grant and learns when it changes or ends.
///
/// Dropping it ends the lease with [`Reason::Dropped`]; [`close`](Self::close) says why.
#[derive(Debug)]
pub struct Consumer {
	state: kio::Shared<State>,
	seen: u64,
}

impl Consumer {
	/// The grant as it stands now.
	pub fn grant(&self) -> Grant {
		self.state.read().grant.clone()
	}

	/// Poll for the next update: the new grant, or the reason the lease ended.
	pub fn poll_changed(&mut self, waiter: &kio::Waiter) -> Poll<Result<Grant, Reason>> {
		let seen = self.seen;
		let state = std::task::ready!(
			self.state
				.poll(waiter, |state| ready_if(state.epoch > seen || state.closed.is_some()))
		);
		if let Some(reason) = &state.closed {
			return Poll::Ready(Err(reason.clone()));
		}
		self.seen = state.epoch;
		Poll::Ready(Ok(state.grant.clone()))
	}

	/// Wait for the next update: the new grant, or the reason the lease ended.
	pub async fn changed(&mut self) -> Result<Grant, Reason> {
		kio::wait(|waiter| self.poll_changed(waiter)).await
	}

	/// Poll for the lease ending.
	pub fn poll_closed(&self, waiter: &kio::Waiter) -> Poll<Reason> {
		let state = std::task::ready!(self.state.poll(waiter, |state| ready_if(state.closed.is_some())));
		Poll::Ready(state.closed.clone().expect("waited for a close"))
	}

	/// Wait for the lease to end.
	pub async fn closed(&self) -> Reason {
		kio::wait(|waiter| self.poll_closed(waiter)).await
	}

	/// End the lease with the session's own close classification, consuming the handle.
	pub fn close(self, reason: impl Into<Reason>) {
		self.state.lock().closed.get_or_insert(reason.into());
	}
}

impl Drop for Consumer {
	fn drop(&mut self) {
		self.state.lock().closed.get_or_insert(Reason::Dropped);
	}
}

fn ready_if(condition: bool) -> Poll<()> {
	if condition { Poll::Ready(()) } else { Poll::Pending }
}

#[cfg(test)]
mod tests {
	use super::*;
	use std::future::Future;
	use std::pin::pin;
	use std::task::{Context, Waker};

	fn grant(publish: &str) -> Grant {
		Grant::new([publish.parse().unwrap()].into_iter().collect(), Default::default())
	}

	fn poll<F: Future>(future: F) -> Poll<F::Output> {
		pin!(future).poll(&mut Context::from_waker(Waker::noop()))
	}

	#[test]
	fn update_wakes_the_consumer_once_per_change() {
		let (producer, mut consumer) = Producer::new(grant("a/**"));
		assert_eq!(consumer.grant(), grant("a/**"));
		assert!(poll(consumer.changed()).is_pending());

		producer.update(grant("b/**"));
		assert_eq!(poll(consumer.changed()), Poll::Ready(Ok(grant("b/**"))));
		assert_eq!(consumer.grant(), grant("b/**"));
		assert!(poll(consumer.changed()).is_pending());
	}

	#[test]
	fn revoke_reaches_the_consumer() {
		let (producer, mut consumer) = Producer::new(grant("a/**"));
		assert!(poll(consumer.closed()).is_pending());

		producer.revoke(Reason::Refused);
		assert_eq!(poll(consumer.closed()), Poll::Ready(Reason::Refused));
		assert_eq!(poll(consumer.changed()), Poll::Ready(Err(Reason::Refused)));
	}

	#[test]
	fn dropping_the_producer_revokes() {
		let (producer, consumer) = Producer::new(grant("a/**"));
		drop(producer);
		assert_eq!(poll(consumer.closed()), Poll::Ready(Reason::Dropped));
	}

	#[test]
	fn the_session_close_reaches_the_producer_and_the_first_reason_wins() {
		let (producer, consumer) = Producer::new(grant("a/**"));
		assert!(poll(producer.closed()).is_pending());

		consumer.close("disconnected");
		assert_eq!(
			poll(producer.closed()),
			Poll::Ready(Reason::Session("disconnected".into()))
		);

		// A later revocation or drop changes nothing.
		producer.revoke(Reason::Expired);
	}

	#[test]
	fn dropping_the_consumer_reports_dropped() {
		let (producer, consumer) = Producer::new(grant("a/**"));
		drop(consumer);
		assert_eq!(poll(producer.closed()), Poll::Ready(Reason::Dropped));
	}

	#[test]
	fn reason_round_trips_as_one_string() {
		for (reason, text) in [
			(Reason::Dropped, "\"dropped\""),
			(Reason::Expired, "\"expired\""),
			(Reason::Session("protocol error".into()), "\"protocol error\""),
		] {
			assert_eq!(serde_json::to_string(&reason).unwrap(), text);
			assert_eq!(serde_json::from_str::<Reason>(text).unwrap(), reason);
		}
	}
}
