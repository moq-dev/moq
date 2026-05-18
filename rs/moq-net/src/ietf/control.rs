use std::sync::{Arc, Mutex};

use tokio::sync::Notify;

use crate::{Error, ietf::RequestId};

struct ControlState {
	request_id_next: RequestId,
	/// None means no flow control (draft17 removed MaxRequestId).
	request_id_max: Option<RequestId>,
	request_id_notify: Arc<Notify>,
}

#[derive(Clone)]
pub(super) struct Control {
	state: Arc<Mutex<ControlState>>,
}

impl Control {
	pub fn new(request_id_max: Option<RequestId>, client: bool) -> Self {
		Self {
			state: Arc::new(Mutex::new(ControlState {
				request_id_next: if client { RequestId(0) } else { RequestId(1) },
				request_id_max,
				request_id_notify: Arc::new(Notify::new()),
			})),
		}
	}

	pub fn max_request_id(&self, max: RequestId) {
		let mut state = self.state.lock().unwrap();
		state.request_id_max = Some(max);
		state.request_id_notify.notify_waiters();
	}

	/// Allocate the next request_id, blocking until MAX_REQUEST_ID allows it.
	pub async fn next_request_id(&self) -> Result<RequestId, Error> {
		let timeout = tokio::time::sleep(std::time::Duration::from_secs(10));
		tokio::pin!(timeout);

		loop {
			let notify = {
				let mut state = self.state.lock().unwrap();

				let allowed = match state.request_id_max {
					None => true,
					Some(max) => state.request_id_next < max,
				};

				if allowed {
					return Ok(state.request_id_next.increment());
				}

				state.request_id_notify.clone().notified_owned()
			};

			tokio::select! {
				_ = notify => continue,
				_ = &mut timeout => {
					tracing::warn!("timed out waiting for MAX_REQUEST_ID");
					return Err(Error::Cancel);
				}
			}
		}
	}
}
