//! The tokio runtime moq-tokio runs in: the caller's, else a fallback started on first use.
//!
//! A caller outside tokio, such as a C binding polling futures from its own event
//! loop, gets a process-wide multi-threaded runtime instead of a panic. Its workers
//! drive the sockets, timers, and tasks the entry points create, and wake whatever
//! executor polls the futures. A caller inside tokio never starts it.

use std::sync::OnceLock;

/// Enter the fallback runtime until the guard drops, unless the caller is already in one.
pub(crate) fn enter() -> Option<tokio::runtime::EnterGuard<'static>> {
	match tokio::runtime::Handle::try_current() {
		Ok(_) => None,
		Err(_) => Some(fallback().enter()),
	}
}

/// The caller's runtime, else the fallback, for a handle that outlives this call.
#[cfg(feature = "driver")]
pub(crate) fn handle() -> tokio::runtime::Handle {
	tokio::runtime::Handle::try_current().unwrap_or_else(|_| fallback().handle().clone())
}

fn fallback() -> &'static tokio::runtime::Runtime {
	static FALLBACK: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
	FALLBACK.get_or_init(|| {
		tokio::runtime::Builder::new_multi_thread()
			.enable_all()
			.thread_name("moq-tokio")
			.build()
			.expect("failed to start the fallback tokio runtime")
	})
}

/// Run `future` with [`enter`] held for each poll, since a poll may spawn or make timers.
#[cfg(any(feature = "net", feature = "jemalloc", feature = "mdns"))]
pub(crate) async fn scope<F: Future>(future: F) -> F::Output {
	let mut future = std::pin::pin!(future);
	std::future::poll_fn(|cx| {
		let _runtime = enter();
		future.as_mut().poll(cx)
	})
	.await
}

/// Spawn `future` on the caller's runtime, else the fallback.
#[cfg(any(feature = "net", all(feature = "watch", feature = "_certs")))]
pub(crate) fn spawn<F>(future: F) -> tokio::task::JoinHandle<F::Output>
where
	F: Future + Send + 'static,
	F::Output: Send + 'static,
{
	let _runtime = enter();
	tokio::spawn(future)
}
