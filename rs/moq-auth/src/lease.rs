//! The handle a session holds for the grant that admitted it.
//!
//! Whoever runs the accept loop decides how a session is authorized, builds a
//! [`Producer`], and hands the [`Consumer`] to the session. The session reads the
//! current [`Grant`], waits for it to change, and learns when it is revoked. The
//! producer side is driven by [`Client`](crate::Client) when an auth server answers, or
//! by any in-process logic when the embedder decides itself. No trait, no callbacks.

#[cfg(feature = "tokio")]
use std::sync::Mutex;
use std::task::Poll;
#[cfg(feature = "tokio")]
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::{Bytes, Grant};

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
	/// The auth server answered a grant the relay cannot honor.
	Invalid,
	/// The grant no longer covers the session's original scope.
	Narrowed,
	/// The relay is shutting down.
	Shutdown,
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
			Self::Invalid => "invalid",
			Self::Narrowed => "narrowed",
			Self::Shutdown => "shutdown",
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
			"invalid" => Self::Invalid,
			"narrowed" => Self::Narrowed,
			"shutdown" => Self::Shutdown,
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
	closed: Option<(Reason, Bytes)>,
	/// Bumped on each re-check ask; the producer observes and clears, so a burst
	/// coalesces into one wake.
	revalidate: u64,
}

/// What the grant's clock requires next.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Due {
	/// Ask the decider for a fresh grant.
	Revalidate,
	/// The grant expired before it could be renewed.
	Expired,
}

#[cfg(feature = "tokio")]
#[derive(Debug)]
struct Clock {
	next: Option<tokio::time::Instant>,
	expires: Option<tokio::time::Instant>,
	cadence: Option<Duration>,
	failures: u32,
	revision: u64,
}

#[cfg(feature = "tokio")]
impl Clock {
	fn new(grant: &Grant) -> Self {
		Self {
			// A cadence too far out to schedule is no scheduled re-check; `expires` still bounds the grant.
			next: grant
				.revalidate
				.and_then(|cadence| tokio::time::Instant::now().checked_add(cadence)),
			expires: grant.deadline(),
			cadence: grant.revalidate,
			failures: 0,
			revision: 0,
		}
	}
}

/// The authorizing side of a lease: applies new grants and revokes.
///
/// Dropping it revokes with [`Reason::Dropped`] unless the lease already ended.
#[derive(Debug)]
pub struct Producer {
	state: kio::Shared<State>,
	#[cfg(feature = "tokio")]
	clock: Mutex<Clock>,
	#[cfg(feature = "tokio")]
	clock_changed: tokio::sync::Notify,
}

impl Producer {
	/// Start a lease on `grant`, returning both handles.
	pub fn new(grant: Grant) -> (Self, Consumer) {
		#[cfg(feature = "tokio")]
		let clock = Mutex::new(Clock::new(&grant));
		let state = kio::Shared::new(State {
			grant,
			epoch: 0,
			closed: None,
			revalidate: 0,
		});
		(
			Self {
				state: state.clone(),
				#[cfg(feature = "tokio")]
				clock,
				#[cfg(feature = "tokio")]
				clock_changed: tokio::sync::Notify::new(),
			},
			Consumer { state, seen: 0 },
		)
	}

	/// Replace the grant, waking the consumer. A no-op once the lease ended.
	pub fn update(&self, grant: Grant) {
		let mut state = self.state.lock();
		if state.closed.is_some() {
			return;
		}
		#[cfg(feature = "tokio")]
		{
			let mut clock = self.clock.lock().expect("lease clock");
			let revision = clock.revision.wrapping_add(1);
			*clock = Clock::new(&grant);
			clock.revision = revision;
		}
		state.grant = grant;
		state.epoch += 1;
		drop(state);
		#[cfg(feature = "tokio")]
		self.clock_changed.notify_waiters();
	}

	/// Wait until the grant needs a re-check or expires. A session may request an
	/// earlier re-check through [`Consumer::revalidate`].
	#[cfg(feature = "tokio")]
	pub async fn due(&self) -> Due {
		loop {
			let changed = self.clock_changed.notified();
			tokio::pin!(changed);
			changed.as_mut().enable();
			let (next, expires, revision) = {
				let clock = self.clock.lock().expect("lease clock");
				(clock.next, clock.expires, clock.revision)
			};
			let revalidate = async {
				match next {
					Some(at) => tokio::time::sleep_until(at).await,
					None => std::future::pending().await,
				}
			};
			let expire = async {
				match expires {
					Some(at) => tokio::time::sleep_until(at).await,
					None => std::future::pending().await,
				}
			};
			tokio::select! {
				biased;
				() = expire => {
					if self.clock.lock().expect("lease clock").revision == revision {
						return Due::Expired;
					}
				},
				() = changed => continue,
				() = revalidate => {
					let mut clock = self.clock.lock().expect("lease clock");
					if clock.revision == revision {
						clock.next = None;
						return Due::Revalidate;
					}
				},
				() = self.revalidate_requested() => {
					self.clock.lock().expect("lease clock").next = None;
					return Due::Revalidate;
				},
			}
		}
	}

	/// Keep the current grant after a failed re-check and schedule a jittered retry.
	#[cfg(feature = "tokio")]
	pub fn failed(&self) -> Duration {
		use rand::RngExt;
		const BACKOFF_MAX: Duration = Duration::from_secs(60);
		let mut clock = self.clock.lock().expect("lease clock");
		clock.failures = clock.failures.saturating_add(1);
		let base = Duration::from_secs(1) * 2u32.saturating_pow(clock.failures.saturating_sub(1).min(16));
		let base = base.min(clock.cadence.unwrap_or(BACKOFF_MAX)).min(BACKOFF_MAX);
		let delay = base.mul_f64(rand::rng().random_range(0.75..=1.25));
		clock.next = Some(tokio::time::Instant::now() + delay);
		clock.revision = clock.revision.wrapping_add(1);
		drop(clock);
		self.clock_changed.notify_waiters();
		delay
	}

	/// Consume an already-asked re-check after an in-flight response.
	#[cfg(feature = "client")]
	pub(crate) fn immediate(&self) {
		let mut clock = self.clock.lock().expect("lease clock");
		clock.next = None;
		clock.revision = clock.revision.wrapping_add(1);
		drop(clock);
		self.clock_changed.notify_waiters();
	}

	/// End the lease with `reason`, consuming the handle, and return the reason
	/// the lease ended with: `reason`, or the consumer's if it closed first.
	pub fn revoke(self, reason: Reason) -> Reason {
		self.finish(reason, Bytes::default()).0
	}

	/// Poll for the lease ending, from either side: why it ended, and the totals
	/// the session reported. A producer-side revoke, or a drop, reports zero bytes.
	pub fn poll_closed(&self, waiter: &kio::Waiter) -> Poll<(Reason, Bytes)> {
		let state = std::task::ready!(self.state.poll(waiter, |state| ready_if(state.closed.is_some())));
		Poll::Ready(state.closed.clone().expect("waited for a close"))
	}

	/// Wait for the lease to end, from either side.
	pub async fn closed(&self) -> (Reason, Bytes) {
		kio::wait(|waiter| self.poll_closed(waiter)).await
	}

	/// Poll until at least one re-check has been asked since the last observation.
	/// A burst of asks resolves once.
	pub fn poll_revalidate(&self, waiter: &kio::Waiter) -> Poll<()> {
		let mut state = std::task::ready!(self.state.poll(waiter, |state| ready_if(state.revalidate > 0)));
		state.revalidate = 0;
		Poll::Ready(())
	}

	/// Wait until a re-check has been asked since the last observation.
	pub async fn revalidate_requested(&self) {
		kio::wait(|waiter| self.poll_revalidate(waiter)).await
	}

	pub(crate) fn finish(&self, reason: Reason, bytes: Bytes) -> (Reason, Bytes) {
		self.state.lock().closed.get_or_insert((reason, bytes)).clone()
	}
}

impl Drop for Producer {
	fn drop(&mut self) {
		self.finish(Reason::Dropped, Bytes::default());
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
	/// A lease on a grant nobody drives: it never changes and is never revoked,
	/// so only the holder ends it. What a static or public grant admits under.
	pub fn fixed(grant: Grant) -> Self {
		let state = kio::Shared::new(State {
			grant,
			epoch: 0,
			closed: None,
			revalidate: 0,
		});
		Self { state, seen: 0 }
	}

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
		if let Some((reason, _)) = &state.closed {
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
		Poll::Ready(state.closed.as_ref().expect("waited for a close").0.clone())
	}

	/// Wait for the lease to end.
	pub async fn closed(&self) -> Reason {
		kio::wait(|waiter| self.poll_closed(waiter)).await
	}

	/// Ask the producer to re-check now. A no-op on a [`fixed`](Self::fixed)
	/// lease, which has nobody to ask.
	pub fn revalidate(&self) {
		self.state.lock().revalidate += 1;
	}

	/// End the lease with the session's own close classification and the totals it
	/// moved, consuming the handle, and return the reason the lease ended with:
	/// `reason`, or the producer's if it revoked first. [`Drop`] reports zero bytes.
	pub fn close(self, reason: impl Into<Reason>, bytes: Bytes) -> Reason {
		self.state.lock().closed.get_or_insert((reason.into(), bytes)).0.clone()
	}
}

impl Drop for Consumer {
	fn drop(&mut self) {
		self.state
			.lock()
			.closed
			.get_or_insert((Reason::Dropped, Bytes::default()));
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
	use std::time::SystemTime;

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
	fn the_session_close_hands_over_byte_totals() {
		let (producer, consumer) = Producer::new(grant("a/**"));
		consumer.close("done", Bytes { sent: 3, received: 5 });
		assert_eq!(
			poll(producer.closed()),
			Poll::Ready((Reason::Session("done".into()), Bytes { sent: 3, received: 5 }))
		);
	}

	#[test]
	fn dropping_the_producer_revokes() {
		let (producer, consumer) = Producer::new(grant("a/**"));
		drop(producer);
		assert_eq!(poll(consumer.closed()), Poll::Ready(Reason::Dropped));
	}

	#[test]
	fn dropping_the_consumer_reports_zero_bytes() {
		let (producer, consumer) = Producer::new(grant("a/**"));
		drop(consumer);
		assert_eq!(
			poll(producer.closed()),
			Poll::Ready((Reason::Dropped, Bytes::default()))
		);
	}

	#[test]
	fn the_session_close_reaches_the_producer_and_the_first_reason_wins() {
		let (producer, consumer) = Producer::new(grant("a/**"));
		assert!(poll(producer.closed()).is_pending());

		let recorded = consumer.close("disconnected", Bytes { sent: 1, received: 2 });
		assert_eq!(recorded, Reason::Session("disconnected".into()));
		assert_eq!(
			poll(producer.closed()),
			Poll::Ready((Reason::Session("disconnected".into()), Bytes { sent: 1, received: 2 }))
		);

		// A later revocation changes nothing, and says so.
		assert_eq!(producer.revoke(Reason::Expired), Reason::Session("disconnected".into()));
	}

	#[test]
	fn a_fixed_lease_only_ends_by_the_holder() {
		let mut consumer = Consumer::fixed(grant("a/**"));
		assert_eq!(consumer.grant(), grant("a/**"));
		assert!(poll(consumer.changed()).is_pending());
		assert!(poll(consumer.closed()).is_pending());
		consumer.revalidate();
		assert_eq!(consumer.grant(), grant("a/**"));
		assert!(poll(consumer.changed()).is_pending());
		assert!(poll(consumer.closed()).is_pending());
		assert_eq!(consumer.close("done", Bytes::default()), Reason::Session("done".into()));
	}

	#[test]
	fn n_nudges_wake_the_producer_once() {
		let (producer, consumer) = Producer::new(grant("a/**"));
		assert!(poll(producer.revalidate_requested()).is_pending());

		for _ in 0..8 {
			consumer.revalidate();
		}
		assert_eq!(poll(producer.revalidate_requested()), Poll::Ready(()));
		assert!(poll(producer.revalidate_requested()).is_pending());

		consumer.revalidate();
		assert_eq!(poll(producer.revalidate_requested()), Poll::Ready(()));
	}

	#[cfg(feature = "tokio")]
	#[tokio::test]
	async fn due_follows_cadence_nudges_and_backoff() {
		tokio::time::pause();
		let mut grant = grant("a/**");
		grant.revalidate = Some(Duration::from_secs(10));
		grant.expires = Some(SystemTime::now() + Duration::from_secs(60));
		let (producer, consumer) = Producer::new(grant.clone());
		assert!(tokio::time::timeout(Duration::ZERO, producer.due()).await.is_err());
		tokio::time::advance(Duration::from_secs(10)).await;
		assert_eq!(producer.due().await, Due::Revalidate);
		assert!(tokio::time::timeout(Duration::ZERO, producer.due()).await.is_err());

		let delay = producer.failed();
		assert!(delay >= Duration::from_millis(750) && delay <= Duration::from_millis(1250));
		tokio::time::advance(Duration::from_secs(2)).await;
		assert_eq!(producer.due().await, Due::Revalidate);

		grant.revalidate = Some(Duration::from_secs(5));
		producer.update(grant);
		consumer.revalidate();
		assert_eq!(producer.due().await, Due::Revalidate);
		assert!(tokio::time::timeout(Duration::ZERO, producer.due()).await.is_err());
	}

	#[cfg(feature = "tokio")]
	#[tokio::test]
	async fn due_reschedules_an_outstanding_wait_after_update() {
		tokio::time::pause();
		let mut grant = grant("a/**");
		grant.revalidate = Some(Duration::from_secs(30));
		grant.expires = Some(SystemTime::now() + Duration::from_secs(3600));
		let (producer, _consumer) = Producer::new(grant.clone());
		let producer = std::sync::Arc::new(producer);
		let waiter = tokio::spawn({
			let producer = producer.clone();
			async move { producer.due().await }
		});
		tokio::task::yield_now().await;
		grant.revalidate = Some(Duration::from_secs(5));
		producer.update(grant);
		tokio::time::advance(Duration::from_secs(5)).await;
		assert_eq!(waiter.await.unwrap(), Due::Revalidate);
	}

	#[cfg(feature = "tokio")]
	#[tokio::test]
	async fn backoff_grows_and_stays_bounded() {
		tokio::time::pause();
		let mut grant = grant("a/**");
		grant.revalidate = Some(Duration::from_secs(30));
		grant.expires = Some(SystemTime::now() + Duration::from_secs(3600));
		let (producer, _consumer) = Producer::new(grant);
		let first = producer.failed();
		assert!(first >= Duration::from_millis(750) && first <= Duration::from_millis(1250));
		let mut later = first;
		for _ in 0..40 {
			later = producer.failed();
		}
		assert!(later <= Duration::from_secs(30).mul_f64(1.25));
	}

	#[cfg(feature = "tokio")]
	#[tokio::test]
	async fn an_unschedulable_cadence_never_rechecks() {
		tokio::time::pause();
		let mut grant = grant("a/**");
		grant.revalidate = Some(Duration::MAX);
		let (producer, _consumer) = Producer::new(grant);
		tokio::time::advance(Duration::from_secs(3600)).await;
		assert!(tokio::time::timeout(Duration::ZERO, producer.due()).await.is_err());
	}

	#[cfg(feature = "tokio")]
	#[tokio::test]
	async fn due_expires_even_without_a_recheck() {
		tokio::time::pause();
		let mut grant = grant("a/**");
		grant.expires = Some(SystemTime::now() - Duration::from_secs(4));
		let (producer, _consumer) = Producer::new(grant);
		let task = tokio::spawn(async move { producer.due().await });
		tokio::task::yield_now().await;
		tokio::time::advance(Duration::from_secs(2)).await;
		assert_eq!(task.await.unwrap(), Due::Expired);
	}

	#[test]
	fn reason_round_trips_as_one_string() {
		for (reason, text) in [
			(Reason::Dropped, "\"dropped\""),
			(Reason::Expired, "\"expired\""),
			(Reason::Refused, "\"refused\""),
			(Reason::Invalid, "\"invalid\""),
			(Reason::Narrowed, "\"narrowed\""),
			(Reason::Shutdown, "\"shutdown\""),
			(Reason::Session("protocol error".into()), "\"protocol error\""),
		] {
			assert_eq!(serde_json::to_string(&reason).unwrap(), text);
			assert_eq!(serde_json::from_str::<Reason>(text).unwrap(), reason);
		}
	}
}
