//! The queue that carries an endpoint's events to one connection's driver.
//!
//! A channel of its own because the connection count multiplies what an idle one holds.
//! tokio's unbounded channel allocates a 32-slot block when it is created, about 7 KiB for
//! this event, and keeps it until the channel drops. This queue allocates nothing until the
//! first event and gives the storage back once a burst is over, yet a busy connection, whose
//! every batch is a few events, keeps its buffer and so allocates nothing per datagram.

use std::{
	collections::VecDeque,
	sync::Arc,
	task::{Context, Poll, Waker},
};

use crate::quic::driver::{ConnectionEvent, mutex::Mutex};

/// What an idle queue keeps: a batch of this many events or fewer never reallocates, and the
/// queue shrinks back to it, so a connection holds about this many events while it idles.
const IDLE_EVENTS: usize = 4;

/// The sending half, held by the endpoint. Dropping it closes the queue once it is drained.
#[derive(Debug)]
pub(crate) struct Sender(Arc<Mutex<State>>);

/// The receiving half, held by the connection's driver.
#[derive(Debug)]
pub(crate) struct Receiver {
	shared: Arc<Mutex<State>>,
	/// Events taken since the receiver last found the queue empty.
	batch: usize,
}

/// The receiver is gone.
#[derive(Debug)]
pub(crate) struct Closed;

#[derive(Debug)]
enum State {
	Open {
		queue: VecDeque<ConnectionEvent>,
		/// The parked receiver, taken by the send that wakes it.
		waker: Option<Waker>,
	},
	/// The sender dropped: the receiver drains `queue`, then sees the end.
	SenderGone { queue: VecDeque<ConnectionEvent> },
	/// The receiver dropped: nothing will ever read, so `send` refuses and holds nothing.
	ReceiverGone,
}

pub(crate) fn channel() -> (Sender, Receiver) {
	let shared = Arc::new(Mutex::new(State::Open {
		queue: VecDeque::new(),
		waker: None,
	}));
	(Sender(shared.clone()), Receiver { shared, batch: 0 })
}

impl Sender {
	/// Queue `event` and wake the receiver, or hand it back when the receiver is gone.
	pub(crate) fn send(&self, event: ConnectionEvent) -> Result<(), Closed> {
		let waker = {
			let mut state = self.0.lock();
			match &mut *state {
				State::Open { queue, waker } => {
					queue.push_back(event);
					waker.take()
				}
				State::ReceiverGone => return Err(Closed),
				State::SenderGone { .. } => unreachable!("the only sender is alive"),
			}
		};
		// Outside the lock, which the receiver's poll takes.
		if let Some(waker) = waker {
			waker.wake();
		}
		Ok(())
	}
}

impl Drop for Sender {
	fn drop(&mut self) {
		let Some(mut state) = self.0.lock_for_drop() else {
			return;
		};
		let waker = match std::mem::replace(&mut *state, State::ReceiverGone) {
			State::Open { queue, waker } => {
				*state = State::SenderGone { queue };
				waker
			}
			// Nothing was left to read.
			State::ReceiverGone => None,
			State::SenderGone { .. } => unreachable!("the only sender dropped twice"),
		};
		drop(state);
		if let Some(waker) = waker {
			waker.wake();
		}
	}
}

impl Receiver {
	/// The next event, or `None` once the sender dropped and the queue is empty.
	pub(crate) fn poll_recv(&mut self, cx: &mut Context<'_>) -> Poll<Option<ConnectionEvent>> {
		let mut state = self.shared.lock();
		let (queue, waker) = match &mut *state {
			State::Open { queue, waker } => (queue, Some(waker)),
			State::SenderGone { queue } => (queue, None),
			State::ReceiverGone => unreachable!("the only receiver is alive"),
		};

		if let Some(event) = queue.pop_front() {
			self.batch += 1;
			return Poll::Ready(Some(event));
		}

		// Drained. An idle connection may never poll again, so this is the only chance to give
		// back what a burst grew, but a busy connection's batches are as large as its bursts:
		// shrink only after one that fit what an idle queue keeps.
		if self.batch <= IDLE_EVENTS {
			queue.shrink_to(IDLE_EVENTS);
		}
		self.batch = 0;

		match waker {
			Some(waker) => {
				if !waker.as_ref().is_some_and(|w| w.will_wake(cx.waker())) {
					*waker = Some(cx.waker().clone());
				}
				Poll::Pending
			}
			None => Poll::Ready(None),
		}
	}
}

impl Drop for Receiver {
	fn drop(&mut self) {
		if let Some(mut state) = self.shared.lock_for_drop() {
			*state = State::ReceiverGone;
		}
	}
}

#[cfg(test)]
mod tests {
	use std::sync::atomic::{AtomicUsize, Ordering};

	use futures::task::{ArcWake, waker};

	use super::*;
	use crate::quic::driver::VarInt;

	#[derive(Default)]
	struct Wakes(AtomicUsize);

	impl ArcWake for Wakes {
		fn wake_by_ref(this: &Arc<Self>) {
			this.0.fetch_add(1, Ordering::SeqCst);
		}
	}

	fn event() -> ConnectionEvent {
		ConnectionEvent::Close {
			error_code: VarInt::from_u32(0),
			reason: String::new(),
		}
	}

	fn capacity(rx: &Receiver) -> usize {
		match &*rx.shared.lock() {
			State::Open { queue, .. } | State::SenderGone { queue } => queue.capacity(),
			State::ReceiverGone => 0,
		}
	}

	/// Take every queued event, ending on the empty poll that may shrink.
	fn drain(rx: &mut Receiver, cx: &mut Context<'_>) -> usize {
		let mut taken = 0;
		while let Poll::Ready(Some(_)) = rx.poll_recv(cx) {
			taken += 1;
		}
		taken
	}

	#[test]
	fn send_wakes_a_parked_receiver_once() {
		let wakes = Arc::new(Wakes::default());
		let waker = waker(wakes.clone());
		let mut cx = Context::from_waker(&waker);
		let (tx, mut rx) = channel();

		assert!(rx.poll_recv(&mut cx).is_pending());
		tx.send(event()).unwrap();
		tx.send(event()).unwrap();
		assert_eq!(wakes.0.load(Ordering::SeqCst), 1);
		assert_eq!(drain(&mut rx, &mut cx), 2);
	}

	#[test]
	fn sender_drop_wakes_then_ends_after_draining() {
		let wakes = Arc::new(Wakes::default());
		let waker = waker(wakes.clone());
		let mut cx = Context::from_waker(&waker);
		let (tx, mut rx) = channel();

		assert!(rx.poll_recv(&mut cx).is_pending());
		tx.send(event()).unwrap();
		drop(tx);
		assert_eq!(wakes.0.load(Ordering::SeqCst), 1);
		assert!(matches!(rx.poll_recv(&mut cx), Poll::Ready(Some(_))));
		assert!(matches!(rx.poll_recv(&mut cx), Poll::Ready(None)));

		let (tx, mut rx) = channel();
		assert!(rx.poll_recv(&mut cx).is_pending());
		drop(tx);
		assert_eq!(wakes.0.load(Ordering::SeqCst), 2);
		assert!(matches!(rx.poll_recv(&mut cx), Poll::Ready(None)));
	}

	#[test]
	fn send_after_receiver_drop_holds_nothing() {
		let (tx, rx) = channel();
		tx.send(event()).unwrap();
		drop(rx);
		assert!(tx.send(event()).is_err());
		assert!(matches!(&*tx.0.lock(), State::ReceiverGone));
	}

	#[test]
	fn idle_queue_allocates_on_demand_and_shrinks_after_a_burst() {
		let waker = waker(Arc::new(Wakes::default()));
		let mut cx = Context::from_waker(&waker);
		let (tx, mut rx) = channel();
		assert_eq!(capacity(&rx), 0);

		for _ in 0..40 {
			tx.send(event()).unwrap();
		}
		assert!(capacity(&rx) >= 40);
		assert_eq!(drain(&mut rx, &mut cx), 40);
		// The burst was large, so a busy connection keeps its buffer.
		assert!(capacity(&rx) >= 40);

		// An idle one sees a small batch next and gives it back.
		tx.send(event()).unwrap();
		assert_eq!(drain(&mut rx, &mut cx), 1);
		assert_eq!(capacity(&rx), IDLE_EVENTS);

		// Small batches then keep that buffer and never reallocate it.
		for _ in 0..3 {
			for _ in 0..IDLE_EVENTS {
				tx.send(event()).unwrap();
			}
			assert_eq!(drain(&mut rx, &mut cx), IDLE_EVENTS);
			assert_eq!(capacity(&rx), IDLE_EVENTS);
		}
	}

	#[test]
	fn large_batches_keep_their_buffer() {
		let waker = waker(Arc::new(Wakes::default()));
		let mut cx = Context::from_waker(&waker);
		let (tx, mut rx) = channel();

		for _ in 0..3 {
			for _ in 0..64 {
				tx.send(event()).unwrap();
			}
			assert_eq!(drain(&mut rx, &mut cx), 64);
		}
		let kept = capacity(&rx);
		assert!(kept >= 64);
		for _ in 0..64 {
			tx.send(event()).unwrap();
		}
		assert_eq!(drain(&mut rx, &mut cx), 64);
		assert_eq!(capacity(&rx), kept);
	}
}
