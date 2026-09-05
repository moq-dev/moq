//! Per-call cancellation, for bindings whose calls block a thread.

use std::future::Future;
use std::sync::Arc;

use crate::error::MoqError;

/// A cancellation token handed to a single blocking call.
///
/// Cancelling it aborts that one call, which reports [`MoqError::Cancelled`], and leaves the
/// object the call was made on usable. Bindings with native async cancellation never need one
/// and pass nothing; it exists for Go, whose generated calls block a goroutine that has no
/// other way to be interrupted.
///
/// Cancelling is permanent, so a token is worth one call: a call started with an already
/// cancelled token fails without running.
#[derive(uniffi::Object)]
pub struct MoqCancel {
	cancelled: tokio::sync::watch::Sender<bool>,
}

#[uniffi::export]
impl MoqCancel {
	/// Create a token that has not been cancelled.
	#[uniffi::constructor]
	pub fn new() -> Arc<Self> {
		Arc::new(Self {
			cancelled: tokio::sync::watch::Sender::new(false),
		})
	}

	/// Abort the call holding this token, or a no-op once that call has returned.
	pub fn cancel(&self) {
		// send_replace, not send: `send` refuses to store the value while no receiver exists,
		// so cancelling before the call reaches `guard` would silently no-op.
		self.cancelled.send_replace(true);
	}
}

/// Race `future` against `cancel`, reporting a cancel as [`MoqError::Cancelled`].
///
/// The future is dropped on cancel, so whatever it registered (a subscription, a fetch,
/// a spawned task) unwinds with it rather than outliving the caller that gave up.
pub(crate) async fn guard<F, T>(cancel: Option<Arc<MoqCancel>>, future: F) -> Result<T, MoqError>
where
	F: Future<Output = Result<T, MoqError>>,
{
	let Some(cancel) = cancel else { return future.await };

	let mut cancelled = cancel.cancelled.subscribe();
	tokio::select! {
		biased;
		Ok(_) = cancelled.wait_for(|&c| c) => Err(MoqError::Cancelled),
		result = future => result,
	}
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
	use std::sync::atomic::{AtomicBool, Ordering};
	use std::time::Duration;

	use super::*;

	#[tokio::test]
	async fn cancel_aborts_a_pending_call() {
		let cancel = MoqCancel::new();
		let token = cancel.clone();
		tokio::spawn(async move {
			tokio::time::sleep(Duration::from_millis(10)).await;
			token.cancel();
		});

		let err = guard(Some(cancel), std::future::pending::<Result<(), MoqError>>())
			.await
			.expect_err("a cancelled call should fail");
		assert!(matches!(err, MoqError::Cancelled));
	}

	#[tokio::test]
	async fn a_cancelled_token_never_runs_the_call() {
		let cancel = MoqCancel::new();
		cancel.cancel();

		let polled = Arc::new(AtomicBool::new(false));
		let flag = polled.clone();
		let err = guard(Some(cancel), async move {
			flag.store(true, Ordering::SeqCst);
			Ok::<(), MoqError>(())
		})
		.await
		.expect_err("a pre-cancelled call should fail");

		assert!(matches!(err, MoqError::Cancelled));
		assert!(!polled.load(Ordering::SeqCst));
	}

	#[tokio::test]
	async fn a_finished_call_ignores_a_later_cancel() {
		let cancel = MoqCancel::new();
		let value = guard(Some(cancel.clone()), async { Ok::<_, MoqError>(7) })
			.await
			.expect("the call should finish");
		assert_eq!(value, 7);

		// Cancelling afterwards is inert rather than an error the next caller sees.
		cancel.cancel();
	}

	#[tokio::test]
	async fn no_token_means_no_cancellation() {
		let value = guard(None, async { Ok::<_, MoqError>(7) })
			.await
			.expect("the call should finish");
		assert_eq!(value, 7);
	}
}
