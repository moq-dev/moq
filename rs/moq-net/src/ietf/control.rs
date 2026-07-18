use std::task::Poll;

use crate::{Error, ietf::RequestId};

struct ControlState {
	request_id_next: RequestId,
	/// None means no flow control (draft17 removed MaxRequestId).
	request_id_max: Option<RequestId>,
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
			}),
		}
	}

	pub fn max_request_id(&self, max: RequestId) {
		// Mutating through the guard wakes any `next_request_id` waiting on the limit.
		self.state.lock().request_id_max = Some(max);
	}

	/// Allocate the next request_id, blocking until MAX_REQUEST_ID allows it.
	pub async fn next_request_id(&self) -> Result<RequestId, Error> {
		let mut timeout = std::pin::pin!(crate::time::sleep(std::time::Duration::from_secs(10)));

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

			if waiter.poll_future(timeout.as_mut()).is_ready() {
				tracing::warn!("timed out waiting for MAX_REQUEST_ID");
				return Poll::Ready(Err(Error::Cancel));
			}

			Poll::Pending
		})
		.await
	}
}
