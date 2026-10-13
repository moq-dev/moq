use std::{collections::BTreeMap, task::Poll};

use crate::{
	Error, SessionError,
	ietf::{RequestId, Version},
};

/// The peer's spent request IDs: every ID of its parity below `next`, except the skipped
/// `start..end` runs in `missing`.
///
/// Independent streams can reorder, so a peer's IDs need not arrive in sequence. A jump
/// costs one run however far it skips, and only a request landing inside a run splits it,
/// so the runs never outnumber the requests received.
struct Spent {
	next: u64,
	missing: BTreeMap<u64, u64>,
}

impl Spent {
	fn new(first: u64) -> Self {
		Self {
			next: first,
			missing: BTreeMap::new(),
		}
	}

	/// The skipped run holding `id`, if any.
	fn run(&self, id: u64) -> Option<(u64, u64)> {
		let (&start, &end) = self.missing.range(..=id).next_back()?;
		(id < end).then_some((start, end))
	}

	/// Whether `id` has the peer's parity and was never spent.
	///
	/// No peer reaches 2^62 requests, and refusing the IDs past it keeps `next` from
	/// overflowing on a draft-17+ varint, which can carry a full 64 bits.
	fn unused(&self, id: u64) -> bool {
		id < 1 << 62 && id % 2 == self.next % 2 && (id >= self.next || self.run(id).is_some())
	}

	/// Whether spending `id` would record one more run.
	fn splits(&self, id: u64) -> bool {
		match self.run(id) {
			Some((start, end)) => start < id && id + 2 < end,
			None => id > self.next,
		}
	}

	/// Spend an [unused](Self::unused) `id`.
	fn spend(&mut self, id: u64) {
		if let Some((start, end)) = self.run(id) {
			self.missing.remove(&start);
			if start < id {
				self.missing.insert(start, id);
			}
			if id + 2 < end {
				self.missing.insert(id + 2, end);
			}
		} else if id > self.next {
			self.missing.insert(self.next, id);
		}
		self.next = self.next.max(id + 2);
	}
}

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
	/// The peer's spent IDs on draft 17 and later, which has no window to bound the runs.
	modern: Spent,
	/// The most runs `modern` may hold. Forgetting one would make its IDs usable again.
	history: u64,
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
	/// Every accepted ID, preserving IDs whose requests retired.
	spent: Spent,
	/// Draft 16 can reorder across streams; drafts 14 and 15 require the next ID.
	reorder: bool,
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
				// The peer takes the other parity: a client's IDs are even, a server's odd.
				modern: Spent::new(u64::from(client)),
				history: crate::session::Limits::default().requests(),
			}),
		}
	}

	/// Flow control the peer's requests (drafts 14 to 16), admitting `window` at once.
	///
	/// `client` is our own role, as passed to [`Self::new`]; the window starts at what
	/// [`initial_max_request_id`] advertised in our SETUP.
	pub fn with_window(self, window: u64, client: bool, version: Version) -> Self {
		let max = initial_max_request_id(window, !client);
		self.state.lock().incoming = Some(Incoming {
			window,
			max,
			sent: max,
			live: 0,
			retired: 0,
			spent: Spent::new(u64::from(client)),
			reorder: version == Version::Draft16,
		});
		self
	}

	/// Bound the runs of skipped IDs a draft-17+ peer may leave, closing the session with
	/// INTERNAL_ERROR past `limit` rather than forgetting any.
	pub fn with_history(self, limit: u64) -> Self {
		self.state.lock().history = limit;
		self
	}

	pub fn max_request_id(&self, max: RequestId) {
		// Mutating through the guard wakes any `next_request_id` waiting on the limit.
		self.state.lock().request_id_max = Some(max);
	}

	/// Admit a request the peer opened, held open until the returned permit drops.
	///
	/// Closes the session with TOO_MANY_REQUESTS when the ID is at or past the
	/// MAX_REQUEST_ID we advertised, or its live requests exceed the window.
	/// Drafts 14/15 require the next ID; draft 16 permits cross-stream reordering.
	/// Used IDs close with INVALID_REQUEST_ID even after retirement, on every draft.
	/// `None` when the peer's requests are not flow controlled (draft 17 and later).
	pub fn accept(&self, id: RequestId) -> Result<Option<RequestPermit>, Error> {
		let mut state = self.state.lock();
		let state = &mut *state;
		let Some(incoming) = &mut state.incoming else {
			let modern = &mut state.modern;
			if !modern.unused(id.0) {
				return Err(SessionError::InvalidRequestId.into());
			}
			if modern.splits(id.0) && modern.missing.len() as u64 >= state.history {
				tracing::warn!(limit = state.history, "request ID history exhausted");
				return Err(SessionError::Internal.into());
			}
			modern.spend(id.0);
			return Ok(None);
		};
		if id.0 >= incoming.max {
			tracing::warn!(id = %id, max = incoming.max, live = incoming.live, "peer exceeded MAX_REQUEST_ID");
			return Err(SessionError::TooManyRequests.into());
		}
		let unused = match incoming.reorder {
			true => incoming.spent.unused(id.0),
			false => id.0 == incoming.spent.next,
		};
		if !unused {
			return Err(SessionError::InvalidRequestId.into());
		}
		if incoming.live >= incoming.window {
			return Err(SessionError::TooManyRequests.into());
		}
		incoming.spent.spend(id.0);
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
		let control = Control::new(None, false).with_window(4, false, Version::Draft15);
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

	/// An ID remains spent after its request ends, across every request type.
	#[test]
	fn retired_or_lower_request_ids_are_refused() {
		for later in [0, 2] {
			let control = Control::new(None, false).with_window(8, false, Version::Draft15);
			for id in [0, 2, 4] {
				drop(control.accept(RequestId(id)).unwrap());
			}
			let err = control.accept(RequestId(later)).err().expect("spent ID refused");
			assert_eq!(SessionError::from(&err).to_code(), 0x4);
		}
	}

	#[test]
	fn a_duplicate_is_invalid_even_when_the_window_is_full() {
		for client in [false, true] {
			let control = Control::new(None, client).with_window(1, client, Version::Draft15);
			let id = RequestId(u64::from(client));
			let _held = control.accept(id).unwrap();
			assert!(matches!(
				control.accept(id),
				Err(Error::Session(SessionError::InvalidRequestId))
			));
		}
	}

	/// The first ID and every subsequent request must be exactly the next in sequence.
	#[test]
	fn skipped_and_wrong_parity_request_ids_are_refused() {
		for client in [false, true] {
			let first = u64::from(client);
			for prefix in [0, 1, 2] {
				for gap in [2, 4, 1] {
					let control = Control::new(None, client).with_window(8, client, Version::Draft15);
					for index in 0..prefix {
						drop(control.accept(RequestId(first + 2 * index)).unwrap());
					}
					let err = control
						.accept(RequestId(first + 2 * prefix + gap))
						.err()
						.expect("non-sequential ID refused");
					assert_eq!(SessionError::from(&err), SessionError::InvalidRequestId);
				}
			}
		}
	}

	#[test]
	fn draft16_independent_streams_can_arrive_out_of_order() {
		for client in [false, true] {
			let control = Control::new(None, client).with_window(8, client, Version::Draft16);
			let first = u64::from(client);
			for id in [first + 2, first, first + 6, first + 4] {
				drop(control.accept(RequestId(id)).expect("independent stream arrival order"));
			}
			for id in [first, first + 2, first + 4, first + 6] {
				assert!(matches!(
					control.accept(RequestId(id)),
					Err(Error::Session(SessionError::InvalidRequestId))
				));
			}
		}
	}

	#[test]
	fn draft16_remembers_only_missing_ids_while_other_requests_retire() {
		let control = Control::new(None, false).with_window(4, false, Version::Draft16);
		for id in 1..1024 {
			drop(control.accept(RequestId(id * 2)).unwrap());
			let state = control.state.lock();
			let missing = &state.incoming.as_ref().unwrap().spent.missing;
			assert_eq!(missing.iter().collect::<Vec<_>>(), [(&0, &2)]);
		}
		drop(control.accept(RequestId(0)).unwrap());
		assert!(control.state.lock().incoming.as_ref().unwrap().spent.missing.is_empty());
		for id in [0, 2, 2046] {
			assert!(matches!(
				control.accept(RequestId(id)),
				Err(Error::Session(SessionError::InvalidRequestId))
			));
		}
	}

	/// Skipping to the top of a production-sized window records one run, not every ID.
	#[test]
	fn draft16_a_jump_is_one_run_split_only_by_requests_inside_it() {
		let window = 220_000;
		let control = Control::new(None, false).with_window(window, false, Version::Draft16);
		let top = 2 * (window - 1);
		let runs = |control: &Control| {
			let state = control.state.lock();
			let missing = &state.incoming.as_ref().unwrap().spent.missing;
			missing.iter().map(|(&start, &end)| (start, end)).collect::<Vec<_>>()
		};

		drop(control.accept(RequestId(top)).unwrap());
		assert_eq!(runs(&control), [(0, top)]);

		for id in [100, 0, top - 2] {
			drop(control.accept(RequestId(id)).unwrap());
		}
		assert_eq!(runs(&control), [(2, 100), (102, top - 2)]);

		for id in [0, 100, top - 2, top] {
			assert!(matches!(
				control.accept(RequestId(id)),
				Err(Error::Session(SessionError::InvalidRequestId))
			));
		}
	}

	#[test]
	fn modern_requests_reject_reuse_and_wrong_parity() {
		for client in [false, true] {
			let control = Control::new(None, client);
			let first = u64::from(client);
			for id in [first + 4, first, first + 2] {
				assert!(control.accept(RequestId(id)).unwrap().is_none());
			}
			for id in [first, first + 2, first + 4, first ^ 1] {
				assert!(
					matches!(
						control.accept(RequestId(id)),
						Err(Error::Session(SessionError::InvalidRequestId))
					),
					"accepted spent or wrong-parity ID {id}"
				);
			}
		}
	}

	/// Contiguous arrival in either order leaves no runs, and a full history refuses a new
	/// gap rather than forgetting an old one.
	#[test]
	fn modern_history_coalesces_and_never_forgets() {
		for reversed in [false, true] {
			let control = Control::new(None, false).with_history(1);
			for n in 0..4096 {
				let id = if reversed { 4095 - n } else { n } * 2;
				control.accept(RequestId(id)).unwrap();
			}
			assert!(control.state.lock().modern.missing.is_empty());
			assert!(matches!(
				control.accept(RequestId(0)),
				Err(Error::Session(SessionError::InvalidRequestId))
			));
		}
		let control = Control::new(None, false).with_history(1);
		control.accept(RequestId(0)).unwrap();
		control.accept(RequestId(4)).unwrap();
		assert!(matches!(
			control.accept(RequestId(8)),
			Err(Error::Session(SessionError::Internal))
		));
		control.accept(RequestId(2)).unwrap();
		control.accept(RequestId(8)).unwrap();
		for id in [0, 2, 4, 8] {
			assert!(matches!(
				control.accept(RequestId(id)),
				Err(Error::Session(SessionError::InvalidRequestId))
			));
		}
	}

	#[test]
	fn modern_sparse_ids_do_not_allocate_the_gap() {
		let control = Control::new(None, false).with_history(1);
		let top = (1 << 62) - 2;
		control.accept(RequestId(top)).unwrap();
		control.accept(RequestId(0)).unwrap();
		let runs: Vec<_> = control.state.lock().modern.missing.clone().into_iter().collect();
		assert_eq!(runs, [(2, top)]);
		for id in [top, 1 << 62, u64::MAX - 1] {
			assert!(matches!(
				control.accept(RequestId(id)),
				Err(Error::Session(SessionError::InvalidRequestId))
			));
		}
	}

	/// Draft-17+ has no MAX_REQUEST_ID: nothing is granted.
	#[test]
	fn unlimited_without_a_window() {
		let control = Control::new(None, true);
		assert!(control.accept(RequestId(1)).unwrap().is_none());
		assert_eq!(granted(&control), None);
	}
}
