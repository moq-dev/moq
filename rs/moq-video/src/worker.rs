//! A codec pinned to one OS thread, driven from anywhere over a channel.
//!
//! Shared by [`encode::Sink`](crate::encode::Sink) and
//! [`decode::Sink`](crate::decode::Sink), which differ only in the requests they
//! serve. The reason both need it is the same: on Windows a Media Foundation
//! transform's COM apartment is per-thread, so the codec has to be created,
//! driven, and dropped on one thread, and a `!Send`-in-practice handle cannot be
//! left to migrate between executor workers or FFI callers.
//!
//! This owns the parts that are easy to get subtly wrong (the open handshake,
//! the cancellation poison, joining on drop). Each codec supplies its own
//! request type and serve loop.

use std::thread::JoinHandle;

use tokio::sync::{mpsc, oneshot};

use crate::Error;

/// The reply channel a worker thread uses to report that its codec is built.
///
/// Sending the name hands the caller a live worker; sending an error fails its
/// `open`. Dropping it without either (the thread panicked) fails it too.
pub(crate) struct Ready(oneshot::Sender<Result<String, Error>>);

impl Ready {
	/// Report the codec as open, under the backend name it resolved to. `false`
	/// means the caller gave up already, so the thread should exit without
	/// serving anything.
	#[must_use]
	pub(crate) fn ok(self, name: &str) -> bool {
		self.0.send(Ok(name.to_string())).is_ok()
	}

	/// Report that the codec could not be built.
	pub(crate) fn err(self, err: Error) {
		let _ = self.0.send(Err(err));
	}
}

/// A codec running on its own thread, and the channel to it.
///
/// `R` is the request type the codec's serve loop matches on.
pub(crate) struct Worker<R> {
	/// `Option` so `Drop` can drop the sender (signalling the thread to exit)
	/// before joining.
	tx: Option<mpsc::UnboundedSender<R>>,
	handle: Option<JoinHandle<()>>,
	name: String,
	/// Set between queueing a request and taking its reply, so a cancelled call
	/// is caught rather than silently skipped. See [`Worker::request`].
	abandoned: bool,
}

impl<R: Send + 'static> Worker<R> {
	/// Spawn `run` on a thread called `thread_name` and wait for it to report
	/// its codec open, so a bad config surfaces here rather than on first use.
	///
	/// `run` owns the whole thread: build the codec, report through [`Ready`],
	/// then serve requests until the receiver closes. Everything it builds is
	/// created and dropped there, which is the point.
	pub(crate) async fn open(
		thread_name: &'static str,
		run: impl FnOnce(Ready, mpsc::UnboundedReceiver<R>) + Send + 'static,
	) -> Result<Self, Error> {
		let (req_tx, req_rx) = mpsc::unbounded_channel::<R>();
		let (ready_tx, ready_rx) = oneshot::channel::<Result<String, Error>>();

		let handle = std::thread::Builder::new()
			.name(thread_name.into())
			.spawn(move || run(Ready(ready_tx), req_rx))
			.map_err(|err| Error::Codec(anyhow::anyhow!("failed to spawn the {thread_name} thread: {err}")))?;

		match ready_rx.await {
			Ok(Ok(name)) => Ok(Self {
				tx: Some(req_tx),
				handle: Some(handle),
				name,
				abandoned: false,
			}),
			Ok(Err(err)) => Err(err),
			Err(_) => {
				let _ = handle.join();
				Err(Error::Codec(anyhow::anyhow!(
					"{thread_name} thread exited before opening"
				)))
			}
		}
	}

	/// The backend name the codec resolved to, e.g. `"mediafoundation"`.
	pub(crate) fn name(&self) -> &str {
		&self.name
	}

	/// Queue a request without waiting for it, mapping a dead thread onto an
	/// error. For work with nothing to report back.
	pub(crate) fn send(&self, req: R) -> Result<(), Error> {
		self.tx.as_ref().ok_or_else(gone)?.send(req).map_err(|_| gone())
	}

	/// Queue a request built around a fresh oneshot and await its reply, mapping
	/// a dead thread onto an error either way.
	///
	/// The flag is what makes a cancelled call safe. Dropping this future after
	/// the send leaves the request queued: the thread still runs it and still
	/// advances the codec, but its reply lands in a dropped receiver. Letting the
	/// next call proceed would produce a stream quietly missing whatever that
	/// request produced, so the worker refuses instead.
	pub(crate) async fn request<T>(
		&mut self,
		build: impl FnOnce(oneshot::Sender<Result<T, Error>>) -> R,
	) -> Result<T, Error> {
		if self.abandoned {
			return Err(abandoned());
		}
		let (resp_tx, resp_rx) = oneshot::channel();
		self.send(build(resp_tx))?;

		self.abandoned = true;
		let reply = resp_rx.await;
		// Only reached if this future was polled to completion; a cancelled one
		// never gets here and leaves the flag set.
		self.abandoned = false;

		reply.map_err(|_| gone())?
	}
}

impl<R> Drop for Worker<R> {
	fn drop(&mut self) {
		// Drop the sender so the thread's `blocking_recv` returns `None` and it
		// exits, dropping the codec on its own thread; then join so teardown (COM
		// uninit) completes before we return. A wedged codec blocking the thread
		// would stall this join, the same tradeoff as the capture pump.
		self.tx.take();
		if let Some(handle) = self.handle.take() {
			let _ = handle.join();
		}
	}
}

fn gone() -> Error {
	Error::Codec(anyhow::anyhow!("codec thread stopped unexpectedly"))
}

fn abandoned() -> Error {
	Error::Codec(anyhow::anyhow!(
		"a cancelled call left the codec ahead of this stream; drop it and open another"
	))
}
