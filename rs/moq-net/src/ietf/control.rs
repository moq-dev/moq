use crate::{Error, SessionError, ietf::RequestId};

struct ControlState {
	/// Our own role.
	client: bool,
	request_id_next: RequestId,
	/// One past the highest ID the peer has used, of its parity.
	///
	/// Request streams can arrive out of order, so any lower ID is still accepted, and a
	/// reused one too: each request owns its stream, so a reuse only confuses the reusing
	/// peer's own joining FETCHes, and remembering every skipped ID would cost memory the
	/// peer chooses.
	peer_next: u64,
	/// A GOAWAY has frozen admission at `peer_next`.
	going_away: bool,
}

#[derive(Clone)]
pub(super) struct Control {
	state: kio::Shared<ControlState>,
}

impl Control {
	pub fn new(client: bool) -> Self {
		Self {
			state: kio::Shared::new(ControlState {
				client,
				request_id_next: if client { RequestId(0) } else { RequestId(1) },
				// The peer takes the other parity: a client's IDs are even, a server's odd.
				peer_next: u64::from(client),
				going_away: false,
			}),
		}
	}

	/// Whether we opened the session.
	pub fn client(&self) -> bool {
		self.state.lock().client
	}

	/// Freeze admission and name the first peer ID past every one it has used.
	///
	/// A lower ID still in flight on a reordered stream is processed as the cutoff promises.
	pub fn goaway(&self) -> Result<RequestId, Error> {
		let mut state = self.state.lock();
		if state.peer_next >= 1 << 62 {
			return Err(SessionError::Internal.into());
		}
		state.going_away = true;
		Ok(RequestId(state.peer_next))
	}

	/// Admit a request the peer opened.
	///
	/// A wrong-parity ID closes with INVALID_REQUEST_ID, and once GOAWAY froze admission, an
	/// ID at or past its cutoff is refused with [`Error::GoingAway`].
	pub fn accept(&self, id: RequestId) -> Result<(), Error> {
		let mut state = self.state.lock();
		// No peer reaches 2^62 requests, and refusing the IDs past it keeps `peer_next` from
		// overflowing on a varint that can carry a full 64 bits.
		if id.0 >= 1 << 62 || id.0 % 2 != state.peer_next % 2 {
			return Err(SessionError::InvalidRequestId.into());
		}
		if id.0 < state.peer_next {
			return Ok(());
		}
		if state.going_away {
			return Err(Error::GoingAway);
		}
		state.peer_next = id.0 + 2;
		Ok(())
	}

	/// Allocate the next request ID.
	pub fn next_request_id(&self) -> RequestId {
		self.state.lock().request_id_next.increment()
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn goaway_freezes_request_admission_at_current_progress() {
		for client in [false, true] {
			let control = Control::new(client);
			let first = u64::from(client);
			assert_eq!(control.goaway().unwrap(), RequestId(first));
			assert!(matches!(control.accept(RequestId(first)), Err(Error::GoingAway)));

			let control = Control::new(client);
			control.accept(RequestId(first)).unwrap();
			control.accept(RequestId(first + 2)).unwrap();
			assert_eq!(control.goaway().unwrap(), RequestId(first + 4));

			// A reordered stream skipped first + 2, which the cutoff still covers.
			let control = Control::new(client);
			control.accept(RequestId(first)).unwrap();
			control.accept(RequestId(first + 4)).unwrap();
			assert_eq!(control.goaway().unwrap(), RequestId(first + 6));
			control.accept(RequestId(first + 2)).unwrap();
			for id in [first + 6, first + 8] {
				assert!(matches!(control.accept(RequestId(id)), Err(Error::GoingAway)));
			}
			assert_eq!(control.goaway().unwrap(), RequestId(first + 6));
		}
	}

	#[test]
	fn requests_need_the_peers_parity() {
		for client in [false, true] {
			let control = Control::new(client);
			let first = u64::from(client);
			for id in [first + 4, first, first + 2, first + 4] {
				control.accept(RequestId(id)).unwrap();
			}
			for id in [first ^ 1, (first + 4) ^ 1] {
				assert!(
					matches!(
						control.accept(RequestId(id)),
						Err(Error::Session(SessionError::InvalidRequestId))
					),
					"accepted wrong-parity ID {id}"
				);
			}
		}
	}

	#[test]
	fn ids_past_2_62_are_refused() {
		let control = Control::new(false);
		let top = (1 << 62) - 2;
		control.accept(RequestId(top)).unwrap();
		control.accept(RequestId(0)).unwrap();
		for id in [1 << 62, u64::MAX - 1] {
			assert!(matches!(
				control.accept(RequestId(id)),
				Err(Error::Session(SessionError::InvalidRequestId))
			));
		}
		assert!(matches!(control.goaway(), Err(Error::Session(SessionError::Internal))));
	}
}
