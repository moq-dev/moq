//! Panic isolation for tasks with an independent owner.
//!
//! A session or binding object can end without ending the process. [`isolate`]
//! reports its task panic to that owner, logging it in the current span and
//! counting it for [`count`].
//!
//! Process-wide tasks that have no independent owner, such as the standalone
//! origin helper, abort instead of leaving a healthy-looking process that serves nothing.

use std::{
	any::Any,
	future::Future,
	panic::AssertUnwindSafe,
	sync::atomic::{AtomicU64, Ordering},
};

use futures::FutureExt;

static COUNT: AtomicU64 = AtomicU64::new(0);

/// Run one owned task, returning its panic to the owner instead of unwinding past it.
///
/// Returns `None` once the panic is logged and counted.
pub async fn isolate<F: Future>(task: F) -> Option<F::Output> {
	match AssertUnwindSafe(task).catch_unwind().await {
		Ok(output) => Some(output),
		Err(payload) => {
			COUNT.fetch_add(1, Ordering::Relaxed);
			tracing::error!(panic = message(payload.as_ref()), "owned task panicked");
			None
		}
	}
}

/// How many panics [`isolate`] has contained since the process started.
pub fn count() -> u64 {
	COUNT.load(Ordering::Relaxed)
}

/// The message a `panic!` carries, for logging.
pub fn message(payload: &(dyn Any + Send)) -> &str {
	match payload.downcast_ref::<&str>() {
		Some(message) => message,
		None => payload
			.downcast_ref::<String>()
			.map_or("<non-string panic>", String::as_str),
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[tokio::test]
	async fn a_panic_is_contained_and_counted() {
		let before = count();
		assert_eq!(isolate(async { 7 }).await, Some(7));
		assert_eq!(count(), before);

		let panicked = isolate(async { panic!("injected") }).await;
		assert_eq!(panicked, None::<()>);
		assert_eq!(count(), before + 1);
	}
}
