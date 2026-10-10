use std::task::Poll;

use crate::{Error, SessionError, ietf::RequestId};

/// The MAX_REQUEST_ID that first admits `window` requests from a peer.
///
/// A client's request IDs are even from 0 and a server's odd from 1, each stepping by 2.
pub(crate) fn initial_max_request_id(window: u64, peer_client: bool) -> u64 {
	let parity = if peer_client { 0 } else { 1 };
	window.saturating_mul(2).saturating_add(parity).min((1 << 62) - 1)
}

struct ControlState {
	request_id_next: RequestId,
	/// None means no flow control (draft17 removed MaxRequestId).
	request_id_max: Option<RequestId>,
	/// The peer's requests, flow controlled on drafts 14 to 16 only.
	incoming: Option<Incoming>,
}

/// The MAX_REQUEST_ID window we grant the peer.
struct Incoming {
	/// How many requests the peer may hold open at once.
	window: u64,
	/// The MAX_REQUEST_ID in force: every request ID the peer uses must be below it.
	max: u64,
	/// The last MAX_REQUEST_ID the peer was told, so each grant goes out once.
	sent: u64,
	/// Requests accepted and not yet retired.
	live: u64,
	/// Requests retired since the last grant.
	retired: u64,
}

#[derive(Clone)]
pub(super) struct Control {
	state: kio::Shared<ControlState>,
}

impl Control {
	pub fn new(request_id_max: Option<RequestId>, client: bool) -> Self {
		Self {
			state: kio::Shared::new(ControlState {
				request_id_next: if client { RequestId(0) } else { RequestId(1) },
				request_id_max,
				incoming: None,
			}),
		}
	}

	/// Flow control the peer's requests (drafts 14 to 16), admitting `window` at once.
	///
	/// `client` is our own role, as passed to [`Self::new`]; the window starts at what
	/// [`initial_max_request_id`] advertised in our SETUP.
	pub fn with_window(self, window: u64, client: bool) -> Self {
		let max = initial_max_request_id(window, !client);
		self.state.lock().incoming = Some(Incoming {
			window,
			max,
			sent: max,
			live: 0,
			retired: 0,
		});
		self
	}

	pub fn max_request_id(&self, max: RequestId) {
		// Mutating through the guard wakes any `next_request_id` waiting on the limit.
		self.state.lock().request_id_max = Some(max);
	}

	/// Admit a request the peer opened, held open until the returned permit drops.
	///
	/// Closes the session with TOO_MANY_REQUESTS when the ID is at or past the
	/// MAX_REQUEST_ID we advertised, or the peer reused IDs to hold more than the window.
	/// `None` when the peer's requests are not flow controlled.
	pub fn accept(&self, id: RequestId) -> Result<Option<RequestPermit>, Error> {
		let mut state = self.state.lock();
		let Some(incoming) = &mut state.incoming else {
			return Ok(None);
		};
		if id.0 >= incoming.max || incoming.live >= incoming.window {
			tracing::warn!(id = %id, max = incoming.max, live = incoming.live, "peer exceeded MAX_REQUEST_ID");
			return Err(SessionError::TooManyRequests.into());
		}
		incoming.live += 1;
		Ok(Some(RequestPermit(self.clone())))
	}

	/// Retire one accepted request, growing the window once half of it has retired.
	///
	/// Batching keeps the MAX_REQUEST_ID traffic a fraction of the requests it grants.
	fn retire(&self) {
		let mut state = self.state.lock();
		let Some(incoming) = &mut state.incoming else {
			return;
		};
		incoming.live -= 1;
		incoming.retired += 1;
		if incoming.retired.saturating_mul(2) >= incoming.window {
			let grant = incoming.retired.saturating_mul(2);
			incoming.max = incoming.max.saturating_add(grant).min((1 << 62) - 1);
			incoming.retired = 0;
		}
	}

	/// Poll for a MAX_REQUEST_ID the peer has not been told yet.
	///
	/// Pending forever when the peer's requests are not flow controlled.
	pub fn poll_grant(&self, waiter: &kio::Waiter) -> Poll<RequestId> {
		let ready = self.state.poll(waiter, |state| match &state.incoming {
			Some(incoming) if incoming.max > incoming.sent => Poll::Ready(()),
			_ => Poll::Pending,
		});
		let mut state = std::task::ready!(ready);
		let incoming = state.incoming.as_mut().expect("checked above");
		incoming.sent = incoming.max;
		Poll::Ready(RequestId(incoming.max))
	}

	/// Allocate the next request_id, blocking until MAX_REQUEST_ID allows it.
	///
	/// `runtime` arms the give-up timer, so the wait cannot outlive the caller's
	/// clock.
	pub async fn next_request_id(&self, runtime: &crate::time::Clock) -> Result<RequestId, Error> {
		let mut timeout = crate::time::Deadline::after(runtime, std::time::Duration::from_secs(10));

		kio::wait(|waiter| {
			let allowed = self.state.poll(waiter, |state| {
				let allowed = match state.request_id_max {
					None => true,
					Some(max) => state.request_id_next < max,
				};
				if allowed { Poll::Ready(()) } else { Poll::Pending }
			});

			if let Poll::Ready(mut state) = allowed {
				return Poll::Ready(Ok(state.request_id_next.increment()));
			}

			if timeout.poll(waiter).is_ready() {
				tracing::warn!("timed out waiting for MAX_REQUEST_ID");
				return Poll::Ready(Err(Error::Cancel));
			}

			Poll::Pending
		})
		.await
	}
}

/// One request the peer holds open, retired (and eventually granted back) on drop.
pub(super) struct RequestPermit(Control);

impl Drop for RequestPermit {
	fn drop(&mut self) {
		self.0.retire();
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	fn granted(control: &Control) -> Option<u64> {
		match control.poll_grant(&kio::Waiter::noop()) {
			Poll::Ready(max) => Some(max.0),
			Poll::Pending => None,
		}
	}

	/// The window admits exactly its size, refuses the next ID with TOO_MANY_REQUESTS,
	/// and grants room back in batches as requests retire.
	#[test]
	fn window_refills_as_requests_retire() {
		// We are the server, so the client's IDs are even: 0, 2, 4, 6.
		let control = Control::new(None, false).with_window(4, false);
		assert_eq!(initial_max_request_id(4, true), 8);

		let mut open: Vec<_> = (0..4)
			.map(|i| control.accept(RequestId(i * 2)).unwrap().unwrap())
			.collect();
		assert!(matches!(
			control.accept(RequestId(8)),
			Err(Error::Session(SessionError::TooManyRequests))
		));
		assert_eq!(granted(&control), None, "nothing retired yet");

		// One retired is below half the window, so nothing is granted yet.
		open.pop();
		assert_eq!(granted(&control), None);

		// Half the window retired: two more IDs.
		open.pop();
		assert_eq!(granted(&control), Some(12));
		assert_eq!(granted(&control), None, "each grant goes out once");

		let _a = control.accept(RequestId(8)).unwrap();
		let _b = control.accept(RequestId(10)).unwrap();
		assert!(control.accept(RequestId(12)).is_err());
	}

	/// A peer reusing request IDs below the window still cannot hold more than it.
	#[test]
	fn reused_ids_cannot_exceed_the_window() {
		let control = Control::new(None, false).with_window(2, false);
		let _a = control.accept(RequestId(0)).unwrap();
		let _b = control.accept(RequestId(0)).unwrap();
		assert!(control.accept(RequestId(2)).is_err());
	}

	/// Draft-17+ has no MAX_REQUEST_ID: nothing is tracked or granted.
	#[test]
	fn unlimited_without_a_window() {
		let control = Control::new(None, true);
		assert!(control.accept(RequestId(u64::MAX)).unwrap().is_none());
		assert_eq!(granted(&control), None);
	}
}
