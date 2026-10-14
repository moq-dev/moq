//! Panic isolation for tasks that serve a single session.
//!
//! A panic unwinds only the task it happens in, so a task serving one session
//! ends that session and nothing else. [`isolate`] is how such a task observes
//! it: logged as an error in the current span and counted for [`count`].
//!
//! A task the process cannot serve without (an origin driver, an accept loop) is
//! never isolated. Running on without it is a process that looks healthy and
//! serves nothing, so its panic has to end the process instead.

use std::{
	any::Any,
	future::Future,
	panic::AssertUnwindSafe,
	sync::atomic::{AtomicU64, Ordering},
};

use futures::FutureExt;

static COUNT: AtomicU64 = AtomicU64::new(0);

/// Run one session's task, ending it on a panic instead of unwinding past it.
///
/// Returns `None` once the panic is logged and counted.
pub async fn isolate<F: Future>(task: F) -> Option<F::Output> {
	match AssertUnwindSafe(task).catch_unwind().await {
		Ok(output) => Some(output),
		Err(payload) => {
			COUNT.fetch_add(1, Ordering::Relaxed);
			tracing::error!(panic = message(payload.as_ref()), "session task panicked");
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
