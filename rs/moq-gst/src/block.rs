use std::sync::Arc;
use std::task::{Context, Poll, Wake};

/// Run `future` to completion on the calling thread, for a state change that must not return early.
///
/// Parks the thread between polls rather than entering an executor, which would panic under a
/// caller's own. A state change from a notify, bus sync, or pad handler runs on a runtime worker,
/// whose queue may hold the very task being waited on (in its LIFO slot, which no other worker can
/// steal), so a multi-thread worker is handed off first and keeps running while this thread blocks.
pub(crate) fn block_on<F: Future>(future: F) -> F::Output {
	let wait = move || {
		let waker = Arc::new(Unpark(std::thread::current())).into();
		let mut cx = Context::from_waker(&waker);
		let mut future = std::pin::pin!(future);
		loop {
			if let Poll::Ready(output) = future.as_mut().poll(&mut cx) {
				return output;
			}
			std::thread::park();
		}
	};
	match tokio::runtime::Handle::try_current().map(|handle| handle.runtime_flavor()) {
		Ok(tokio::runtime::RuntimeFlavor::MultiThread) => tokio::task::block_in_place(wait),
		_ => wait(),
	}
}

/// Wakes the thread parked in [`block_on`].
struct Unpark(std::thread::Thread);

impl Wake for Unpark {
	fn wake(self: Arc<Self>) {
		self.0.unpark();
	}
}
