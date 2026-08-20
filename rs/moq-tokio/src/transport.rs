//! Transitional adapter from the async transport interface to the poll one.
//!
//! moq-net only accepts the poll interface (`web_transport_trait::poll`).
//! Backends that predate it (qmux, iroh, noq) are wrapped in [`Async`] here,
//! at the edge where async already lives, until they implement the poll
//! interface natively upstream. Delete this module backend by backend as that
//! lands: the wrapping costs one allocation per operation and one copy per
//! write, and its closed-watch emulation is weaker than a native
//! implementation (see [`Async`]).

use std::task::{Context, Poll, ready};

use bytes::Bytes;
use futures::FutureExt;
use web_transport_trait::poll as wt_poll;

/// A stored in-flight operation future. Native transports are Send, so the
/// plain boxed flavor suffices.
type OpBox<T> = futures::future::BoxFuture<'static, T>;

/// Adapts a transport that only implements the async interface
/// ([`web_transport_trait::Session`]) to the poll interface this crate requires.
///
/// Each pending session operation is stored as a boxed future built from a clone
/// of the underlying session; stream operations temporarily move the stream into
/// the future and take it back when the operation settles. Two costs follow: one
/// allocation per operation, and one copy per write (a poll write borrows its
/// buffer, an async write future cannot). Prefer a transport's native poll
/// implementation when it has one.
///
/// A few contract loosenings, all inherent to wrapping futures:
///
/// - A write must be retried with the same bytes until it resolves. The adapter
///   copies the buffer it was first given; a caller that swaps buffers while the
///   write is pending would corrupt the stream. Every caller in this crate
///   retries with the same buffer.
/// - `stop`, `reset`, and `finish` during an in-flight operation are deferred
///   until that operation settles. If the stream is dropped before then, the
///   pending operation is finished on the tokio runtime and the deferred code
///   applied after it, so the peer still sees the real reason; only without a
///   runtime does the underlying transport's drop behavior (a reset or stop
///   with code 0) stand in.
/// - Stream `poll_closed` watches are emulated rather than delegated: the
///   underlying async `closed()` future would own the stream for its whole
///   lifetime, deadlocking any later read or write (the async trait's
///   `closed(&mut self)` cannot be stored as a `'static` watch without taking
///   the stream with it). A receive stream reports closed through its reads
///   (FIN drained, or a read error), buffering bounded read-ahead so a FIN
///   behind undelivered payload still resolves the watch; past the cap the
///   watch parks until the caller's reads drain the backlog, deliberately
///   trading watch liveness on an undrained flooding stream for a memory
///   bound (every driver in this crate drains its reads alongside the watch).
///   A send stream only
///   starts the real watch once `finish` or `reset` makes it terminal; before
///   that a closure surfaces as an error on the next write instead of waking an
///   idle watch. The send-side gap this leaves: a peer that resets a stream
///   sitting idle (no pending write, not finished) does not wake a parked
///   `poll_closed`, so a driver waiting on "next frame or peer close" learns of
///   the closure only when the next write fails. The drivers' other arms keep
///   the session making progress; the stream itself lingers until then. A
///   native poll implementation observes closure without owning the stream,
///   which is the real fix and the reason this adapter is transitional.
pub struct Async<S: web_transport_trait::Session> {
	session: S,
	accept_uni: OpSlot<Result<S::RecvStream, S::Error>>,
	accept_bi: OpSlot<BiResult<S>>,
	open_uni: OpSlot<Result<S::SendStream, S::Error>>,
	open_bi: OpSlot<BiResult<S>>,
	recv_datagram: OpSlot<Result<Bytes, S::Error>>,
	closed: OpSlot<S::Error>,
}

/// The stream pair an async-interface accept or open resolves to.
type BiResult<S> = Result<
	(
		<S as web_transport_trait::Session>::SendStream,
		<S as web_transport_trait::Session>::RecvStream,
	),
	<S as web_transport_trait::Session>::Error,
>;

/// A stored in-flight operation: `None` when idle.
///
/// The Mutex is never locked: every access goes through `get_mut`, which is free
/// on an exclusive borrow. It exists only to make the slot `Sync`, since a boxed
/// future is not, and sessions are shared behind `&` between the driver's tasks.
type OpSlot<T> = std::sync::Mutex<Option<OpBox<T>>>;

/// Lazily start and poll the operation in `slot`, clearing it when it resolves.
fn poll_op<T>(slot: &mut OpSlot<T>, cx: &mut Context<'_>, start: impl FnOnce() -> OpBox<T>) -> Poll<T> {
	let slot = slot.get_mut().unwrap();
	let fut = slot.get_or_insert_with(start);
	let output = ready!(fut.as_mut().poll(cx));
	*slot = None;
	Poll::Ready(output)
}

impl<S: web_transport_trait::Session> Async<S> {
	/// Wrap an async-interface transport session.
	pub fn new(session: S) -> Self {
		Self {
			session,
			accept_uni: OpSlot::default(),
			accept_bi: OpSlot::default(),
			open_uni: OpSlot::default(),
			open_bi: OpSlot::default(),
			recv_datagram: OpSlot::default(),
			closed: OpSlot::default(),
		}
	}
}

// Manual impl: a clone starts with no in-flight operations, since each handle
// owns its own progress under the poll contract.
impl<S: web_transport_trait::Session> Clone for Async<S> {
	fn clone(&self) -> Self {
		Self::new(self.session.clone())
	}
}

impl<S> wt_poll::Session for Async<S>
where
	S: web_transport_trait::Session,
	S::SendStream: 'static,
	S::RecvStream: 'static,
{
	type SendStream = AsyncSend<S::SendStream>;
	type RecvStream = AsyncRecv<S::RecvStream>;
	type Error = S::Error;

	fn poll_accept_uni(&mut self, cx: &mut Context<'_>) -> Poll<Result<Self::RecvStream, Self::Error>> {
		let session = &self.session;
		poll_op(&mut self.accept_uni, cx, || {
			let session = session.clone();
			async move { session.accept_uni().await }.boxed()
		})
		.map(|res| res.map(AsyncRecv::new))
	}

	fn poll_accept_bi(&mut self, cx: &mut Context<'_>) -> Poll<Result<wt_poll::BiStreams<Self>, Self::Error>> {
		let session = &self.session;
		poll_op(&mut self.accept_bi, cx, || {
			let session = session.clone();
			async move { session.accept_bi().await }.boxed()
		})
		.map(|res| res.map(|(send, recv)| (AsyncSend::new(send), AsyncRecv::new(recv))))
	}

	fn poll_open_uni(&mut self, cx: &mut Context<'_>) -> Poll<Result<Self::SendStream, Self::Error>> {
		let session = &self.session;
		poll_op(&mut self.open_uni, cx, || {
			let session = session.clone();
			async move { session.open_uni().await }.boxed()
		})
		.map(|res| res.map(AsyncSend::new))
	}

	fn poll_open_bi(&mut self, cx: &mut Context<'_>) -> Poll<Result<wt_poll::BiStreams<Self>, Self::Error>> {
		let session = &self.session;
		poll_op(&mut self.open_bi, cx, || {
			let session = session.clone();
			async move { session.open_bi().await }.boxed()
		})
		.map(|res| res.map(|(send, recv)| (AsyncSend::new(send), AsyncRecv::new(recv))))
	}

	fn poll_send_datagram(&mut self, _cx: &mut Context<'_>, payload: &[u8]) -> Poll<Result<(), Self::Error>> {
		// The async interface's send is non-blocking (the transport queues or
		// drops), so there is never a reason to return Pending.
		Poll::Ready(self.session.send_datagram(Bytes::copy_from_slice(payload)))
	}

	fn poll_recv_datagram(&mut self, cx: &mut Context<'_>) -> Poll<Result<Bytes, Self::Error>> {
		let session = &self.session;
		poll_op(&mut self.recv_datagram, cx, || {
			let session = session.clone();
			async move { session.recv_datagram().await }.boxed()
		})
	}

	fn max_datagram_size(&self) -> usize {
		self.session.max_datagram_size()
	}

	fn protocol(&self) -> Option<&str> {
		self.session.protocol()
	}

	fn close(&mut self, code: u32, reason: &str) {
		self.session.close(code, reason);
	}

	fn poll_closed(&mut self, cx: &mut Context<'_>) -> Poll<Self::Error> {
		let session = &self.session;
		poll_op(&mut self.closed, cx, || {
			let session = session.clone();
			async move { session.closed().await }.boxed()
		})
	}

	fn stats(&self) -> impl web_transport_trait::Stats {
		self.session.stats()
	}
}

/// The send half produced by [`Async`]: an async-interface stream adapted to the
/// poll interface. See [`Async`] for the deferred `finish`/`reset` behavior.
pub struct AsyncSend<S: web_transport_trait::SendStream + 'static> {
	// Same never-locked Mutex as [`Async`]'s slots: `get_mut` is free on an
	// exclusive borrow, and the wrapper keeps the boxed future from stripping
	// `Sync` off the containing session types.
	state: std::sync::Mutex<Option<SendState<S>>>,
	// Deferred actions, applied when the in-flight operation settles.
	priority: Option<u8>,
	finish: bool,
	reset: Option<u32>,
	/// Whether `finish` or `reset` has been observed, so no further writes can
	/// come and the closed() watch may safely take ownership of the stream.
	terminal: bool,
	/// Bytes already transmitted but not yet reported to the caller: a write the
	/// stored future finished (possibly inside
	/// [`poll_closed`](wt_poll::SendStream::poll_closed)) while the caller held a
	/// `Pending`. Later `poll_write` calls reconcile against the buffer actually
	/// presented (the poll contract allows a shorter retry), reporting and
	/// consuming a prefix per call, never writing these bytes a second time.
	completed: Bytes,
	/// Cancels the in-flight write so a reset applies immediately instead of
	/// waiting behind blocked I/O (whose partial progress a reset discards
	/// anyway). Fired by [`reset`](wt_poll::SendStream::reset) and by [`Drop`].
	interrupt: Option<futures::channel::oneshot::Sender<()>>,
}

enum SendState<S: web_transport_trait::SendStream + 'static> {
	Idle(S),
	/// A write in flight; `chunk` is the copied bytes, kept (refcounted, no
	/// extra copy) so a completion absorbed by `poll_closed` can verify the
	/// retrying caller supplied the same bytes. A `None` result means the write
	/// was interrupted by a reset (see [`AsyncSend::interrupt`]).
	Writing {
		#[allow(clippy::type_complexity)]
		fut: OpBox<(S, Option<Result<(), S::Error>>)>,
		chunk: Bytes,
	},
	/// The closed() acknowledgement watch; a `None` result means it was
	/// interrupted by a late reset (see [`AsyncSend::interrupt`]).
	Closing(#[allow(clippy::type_complexity)] OpBox<(S, Option<Result<(), S::Error>>)>),
}

impl<S: web_transport_trait::SendStream + 'static> AsyncSend<S> {
	/// Wrap an async-interface send stream.
	pub fn new(stream: S) -> Self {
		Self {
			state: std::sync::Mutex::new(Some(SendState::Idle(stream))),
			priority: None,
			finish: false,
			reset: None,
			terminal: false,
			completed: Bytes::new(),
			interrupt: None,
		}
	}

	/// Apply actions deferred while an operation was in flight.
	fn settle(&mut self, stream: &mut S) {
		if let Some(order) = self.priority.take() {
			stream.set_priority(order);
		}
		if let Some(code) = self.reset.take() {
			self.finish = false;
			stream.reset(code);
		}
		if std::mem::take(&mut self.finish) {
			// A deferred finish has nowhere to report to; its failure surfaces
			// through closed() like any other terminal state.
			let _ = stream.finish();
		}
	}
}

impl<S: web_transport_trait::SendStream + 'static> wt_poll::SendStream for AsyncSend<S> {
	type Error = S::Error;

	fn poll_write(&mut self, cx: &mut Context<'_>, buf: &[u8]) -> Poll<Result<usize, Self::Error>> {
		loop {
			// Transmitted-but-unreported bytes from a completed write are
			// reconciled against the buffer presented NOW: the poll contract
			// allows a retry with a shorter buffer, so report (and consume) at
			// most its length and keep the rest for the next call.
			if !self.completed.is_empty() && !buf.is_empty() {
				let n = buf.len().min(self.completed.len());
				let reported = self.completed.split_to(n);
				// A hard assert, not a debug one: these bytes are already on the
				// wire and the bridge cannot un-send them, so a caller continuing
				// with different data would silently corrupt the stream. Failing
				// loudly is the only honest option left.
				assert_eq!(
					&buf[..n],
					&reported[..],
					"poll_write must continue with the bytes the write bridge already transmitted"
				);
				return Poll::Ready(Ok(n));
			}
			match self.state.get_mut().unwrap().take().expect("in-flight") {
				SendState::Idle(mut stream) => {
					if buf.is_empty() {
						*self.state.get_mut().unwrap() = Some(SendState::Idle(stream));
						return Poll::Ready(Ok(0));
					}
					// write_chunk delivers the whole chunk, so the reported
					// count is exact. The caller retries with the same bytes
					// until this resolves (see [`Async`]).
					let chunk = Bytes::copy_from_slice(buf);
					let retained = chunk.clone();
					let (tx, rx) = futures::channel::oneshot::channel::<()>();
					self.interrupt = Some(tx);
					let fut = async move {
						let res = {
							let mut op = std::pin::pin!(stream.write_chunk(chunk).fuse());
							let mut rx = rx.fuse();
							loop {
								futures::select_biased! {
									res = op => break Some(res),
									cancel = rx => match cancel {
										Ok(()) => break None,
										// A dropped (unfired) sender must not interrupt;
										// only an explicit reset does. The fused arm goes
										// quiet and the write keeps driving.
										Err(_) => continue,
									},
								}
							}
						};
						(stream, res)
					}
					.boxed();
					*self.state.get_mut().unwrap() = Some(SendState::Writing { fut, chunk: retained });
				}
				SendState::Writing { mut fut, chunk } => match fut.as_mut().poll(cx) {
					Poll::Pending => {
						*self.state.get_mut().unwrap() = Some(SendState::Writing { fut, chunk });
						return Poll::Pending;
					}
					Poll::Ready((mut stream, res)) => {
						self.interrupt = None;
						self.settle(&mut stream);
						*self.state.get_mut().unwrap() = Some(SendState::Idle(stream));
						// An interrupted write was abandoned for a reset, which
						// settle just applied; the next iteration's write reports
						// the stream state.
						if let Some(res) = res {
							res?;
							// Reported through the reconciliation at the top of the
							// loop, which caps it at the presented buffer's length.
							self.completed = chunk;
						}
					}
				},
				SendState::Closing(mut fut) => match fut.as_mut().poll(cx) {
					Poll::Pending => {
						*self.state.get_mut().unwrap() = Some(SendState::Closing(fut));
						return Poll::Pending;
					}
					Poll::Ready((mut stream, _res)) => {
						self.interrupt = None;
						self.settle(&mut stream);
						*self.state.get_mut().unwrap() = Some(SendState::Idle(stream));
					}
				},
			}
		}
	}

	fn set_priority(&mut self, order: u8) {
		match self.state.get_mut().unwrap().as_mut() {
			Some(SendState::Idle(stream)) => stream.set_priority(order),
			_ => self.priority = Some(order),
		}
	}

	fn finish(&mut self) -> Result<(), Self::Error> {
		self.terminal = true;
		match self.state.get_mut().unwrap().as_mut() {
			Some(SendState::Idle(stream)) => stream.finish(),
			_ => {
				self.finish = true;
				Ok(())
			}
		}
	}

	fn reset(&mut self, code: u32) {
		self.terminal = true;
		match self.state.get_mut().unwrap().as_mut() {
			Some(SendState::Idle(stream)) => stream.reset(code),
			_ => {
				self.reset = Some(code);
				// A reset discards the in-flight write's progress anyway, so
				// cancel it rather than letting blocked I/O delay the code.
				if let Some(tx) = self.interrupt.take() {
					let _ = tx.send(());
				}
			}
		}
	}

	fn poll_closed(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
		loop {
			match self.state.get_mut().unwrap().take().expect("in-flight") {
				SendState::Idle(mut stream) => {
					self.settle(&mut stream);
					// Only a finished (or reset) stream starts the real closed()
					// watch: that future owns the stream, and a stream the caller
					// may still write to must stay reclaimable. Before that,
					// closure surfaces as an error on the next operation instead.
					if !self.terminal {
						*self.state.get_mut().unwrap() = Some(SendState::Idle(stream));
						return Poll::Pending;
					}
					// Interruptible like a write: a late reset (the group machines
					// cancel a finished stream this way) must not wait for a FIN
					// acknowledgement that may never come.
					let (tx, rx) = futures::channel::oneshot::channel::<()>();
					self.interrupt = Some(tx);
					let fut = async move {
						let res = {
							let mut op = std::pin::pin!(stream.closed().fuse());
							let mut rx = rx.fuse();
							loop {
								futures::select_biased! {
									res = op => break Some(res),
									cancel = rx => match cancel {
										Ok(()) => break None,
										Err(_) => continue,
									},
								}
							}
						};
						(stream, res)
					}
					.boxed();
					*self.state.get_mut().unwrap() = Some(SendState::Closing(fut));
				}
				SendState::Writing { mut fut, chunk } => match fut.as_mut().poll(cx) {
					Poll::Pending => {
						*self.state.get_mut().unwrap() = Some(SendState::Writing { fut, chunk });
						return Poll::Pending;
					}
					Poll::Ready((mut stream, res)) => {
						self.interrupt = None;
						self.settle(&mut stream);
						*self.state.get_mut().unwrap() = Some(SendState::Idle(stream));
						match res {
							// A failed write is a closed stream; report it here.
							Some(Err(err)) => return Poll::Ready(Err(err)),
							// The caller of poll_write was told Pending and will
							// retry; poll_write's reconciliation reports these bytes
							// instead of writing them a second time.
							Some(Ok(())) => self.completed = chunk,
							// Interrupted for a reset, applied by settle above; the
							// next iteration starts the real closed watch.
							None => {}
						}
					}
				},
				SendState::Closing(mut fut) => match fut.as_mut().poll(cx) {
					Poll::Pending => {
						*self.state.get_mut().unwrap() = Some(SendState::Closing(fut));
						return Poll::Pending;
					}
					Poll::Ready((mut stream, res)) => {
						self.interrupt = None;
						self.settle(&mut stream);
						*self.state.get_mut().unwrap() = Some(SendState::Idle(stream));
						// An interrupted watch was abandoned for a late reset,
						// which settle just applied; the next iteration watches
						// the now-reset stream, which resolves promptly.
						if let Some(res) = res {
							return Poll::Ready(res);
						}
					}
				},
			}
		}
	}
}

impl<S: web_transport_trait::SendStream + 'static> Drop for AsyncSend<S> {
	fn drop(&mut self) {
		match self.state.get_mut().unwrap().take() {
			// Apply a deferred reset if the stream is idle.
			Some(SendState::Idle(mut stream)) => {
				if let Some(code) = self.reset.take() {
					stream.reset(code);
				}
			}
			// The stream lives inside the in-flight future; dropping it here
			// would fire the backend's default drop behavior (a code-0 reset)
			// and lose the deferred reset code or FIN the caller asked for. The
			// peer uses that code to classify the abort (Old vs Cancel vs
			// Evicted), so finish the operation on the runtime and apply the
			// terminal action then. A deferred reset fired the interrupt when it
			// was recorded, so the salvage task never waits on blocked I/O; a
			// deferred finish lets the write complete first (the FIN follows the
			// data). Without a runtime the default drop stands.
			Some(SendState::Writing { fut, .. }) => {
				let reset = self.reset.take();
				let finish = std::mem::take(&mut self.finish);
				if (reset.is_some() || finish)
					&& let Ok(handle) = tokio::runtime::Handle::try_current()
				{
					handle.spawn(async move {
						let (mut stream, _) = fut.await;
						match reset {
							Some(code) => stream.reset(code),
							None => {
								let _ = stream.finish();
							}
						}
					});
				}
			}
			Some(SendState::Closing(fut)) => {
				let reset = self.reset.take();
				if reset.is_some()
					&& let Ok(handle) = tokio::runtime::Handle::try_current()
				{
					handle.spawn(async move {
						let (mut stream, _) = fut.await;
						if let Some(code) = reset {
							stream.reset(code);
						}
					});
				}
			}
			None => {}
		}
	}
}

/// The receive half produced by [`Async`]: an async-interface stream adapted to
/// the poll interface. See [`Async`] for the deferred `stop` behavior.
pub struct AsyncRecv<S: web_transport_trait::RecvStream + 'static> {
	// See [`AsyncSend::state`] for why this Mutex exists; it is never locked.
	state: std::sync::Mutex<Option<RecvState<S>>>,
	/// Bytes already read from the transport but not yet handed to the caller.
	buffer: Bytes,
	/// Read-ahead pulled in by the closed-watch while payload sat undelivered, so
	/// a FIN right behind buffered data still resolves the watch. Drained by the
	/// read methods before the transport is polled again; bounded by
	/// [`READ_AHEAD_CAP`].
	queued: std::collections::VecDeque<Bytes>,
	/// Total bytes across `queued`, so the cap check is O(1).
	queued_len: usize,
	/// The peer finished the stream.
	fin: bool,
	/// A deferred STOP_SENDING, applied when the in-flight operation settles.
	stop: Option<u32>,
	/// Cancels the in-flight read so a stop applies immediately instead of
	/// waiting behind blocked I/O. Fired by [`stop`](wt_poll::RecvStream::stop)
	/// and by [`Drop`].
	interrupt: Option<futures::channel::oneshot::Sender<()>>,
}

/// How much the closed-watch may read ahead of the caller before it parks and
/// waits for the caller to drain. Bounds memory against a peer that floods a
/// stream nobody is reading.
const READ_AHEAD_CAP: usize = 64 * 1024;

enum RecvState<S: web_transport_trait::RecvStream + 'static> {
	Idle(S),
	/// A read in flight; a `None` result means it was interrupted by a stop
	/// (see [`AsyncRecv::interrupt`]).
	Reading(#[allow(clippy::type_complexity)] OpBox<(S, Option<Result<Option<Bytes>, S::Error>>)>),
}

impl<S: web_transport_trait::RecvStream + 'static> AsyncRecv<S> {
	/// Wrap an async-interface receive stream.
	pub fn new(stream: S) -> Self {
		Self {
			state: std::sync::Mutex::new(Some(RecvState::Idle(stream))),
			buffer: Bytes::new(),
			queued: std::collections::VecDeque::new(),
			queued_len: 0,
			fin: false,
			stop: None,
			interrupt: None,
		}
	}

	/// Pop read-ahead into `self.buffer` if the head is empty.
	fn unqueue(&mut self) {
		if self.buffer.is_empty()
			&& let Some(next) = self.queued.pop_front()
		{
			self.queued_len -= next.len();
			self.buffer = next;
		}
	}

	/// Read the next chunk from the transport, setting `self.fin` on the FIN.
	fn poll_fill(&mut self, cx: &mut Context<'_>, max: usize) -> Poll<Result<Option<Bytes>, S::Error>> {
		loop {
			match self.state.get_mut().unwrap().take().expect("in-flight") {
				RecvState::Idle(mut stream) => {
					if let Some(code) = self.stop.take() {
						stream.stop(code);
					}
					let (tx, rx) = futures::channel::oneshot::channel::<()>();
					self.interrupt = Some(tx);
					let fut = async move {
						let res = {
							let mut op = std::pin::pin!(stream.read_chunk(max).fuse());
							let mut rx = rx.fuse();
							loop {
								futures::select_biased! {
									res = op => break Some(res),
									cancel = rx => match cancel {
										Ok(()) => break None,
										// A dropped (unfired) sender must not interrupt;
										// only an explicit stop does. The fused arm goes
										// quiet and the read keeps driving.
										Err(_) => continue,
									},
								}
							}
						};
						(stream, res)
					}
					.boxed();
					*self.state.get_mut().unwrap() = Some(RecvState::Reading(fut));
				}
				RecvState::Reading(mut fut) => match fut.as_mut().poll(cx) {
					Poll::Pending => {
						*self.state.get_mut().unwrap() = Some(RecvState::Reading(fut));
						return Poll::Pending;
					}
					Poll::Ready((mut stream, res)) => {
						self.interrupt = None;
						// Apply a stop requested while the read was in flight now, not on
						// the next read: a caller that stops and never reads again must
						// still send STOP_SENDING.
						if let Some(code) = self.stop.take() {
							stream.stop(code);
						}
						*self.state.get_mut().unwrap() = Some(RecvState::Idle(stream));
						// An interrupted read was abandoned for the stop applied
						// above; the next iteration's read reports the stream state.
						let Some(res) = res else { continue };
						if let Ok(None) = &res {
							self.fin = true;
						}
						return Poll::Ready(res);
					}
				},
			}
		}
	}
}

impl<S: web_transport_trait::RecvStream + 'static> wt_poll::RecvStream for AsyncRecv<S> {
	type Error = S::Error;

	fn poll_read(&mut self, cx: &mut Context<'_>, dst: &mut [u8]) -> Poll<Result<Option<usize>, Self::Error>> {
		if dst.is_empty() {
			return Poll::Ready(Ok(Some(0)));
		}

		loop {
			self.unqueue();
			if !self.buffer.is_empty() {
				let n = dst.len().min(self.buffer.len());
				dst[..n].copy_from_slice(&self.buffer.split_to(n));
				return Poll::Ready(Ok(Some(n)));
			}
			if self.fin {
				return Poll::Ready(Ok(None));
			}
			if let Some(bytes) = ready!(self.poll_fill(cx, dst.len()))? {
				self.buffer = bytes;
			}
		}
	}

	fn poll_read_chunk(&mut self, cx: &mut Context<'_>, max: usize) -> Poll<Result<Option<Bytes>, Self::Error>> {
		if max == 0 {
			return Poll::Ready(Ok(Some(Bytes::new())));
		}

		loop {
			self.unqueue();
			if !self.buffer.is_empty() {
				let n = max.min(self.buffer.len());
				return Poll::Ready(Ok(Some(self.buffer.split_to(n))));
			}
			if self.fin {
				return Poll::Ready(Ok(None));
			}
			if let Some(bytes) = ready!(self.poll_fill(cx, max))? {
				self.buffer = bytes;
			}
		}
	}

	fn stop(&mut self, code: u32) {
		match self.state.get_mut().unwrap().as_mut() {
			Some(RecvState::Idle(stream)) => stream.stop(code),
			_ => {
				self.stop = Some(code);
				// The caller is discarding the stream, so cancel the in-flight
				// read rather than letting blocked I/O delay the stop.
				if let Some(tx) = self.interrupt.take() {
					let _ = tx.send(());
				}
			}
		}
	}

	fn poll_closed(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
		// Emulated with reads rather than the underlying closed(): that future
		// would own the stream, deadlocking any later read (the caller reclaims a
		// dropped watch by simply reading again, which the bridge must preserve).
		// A FIN or reset therefore surfaces through reads; payload arriving before
		// it is queued as read-ahead (up to READ_AHEAD_CAP) so a FIN right behind
		// undelivered data still resolves the watch without an external read.
		loop {
			if self.fin {
				return Poll::Ready(Ok(()));
			}
			if self.buffer.len() + self.queued_len >= READ_AHEAD_CAP {
				// The caller's own reads are what drain the backlog, and their
				// re-poll of this watch is what resumes it.
				return Poll::Pending;
			}
			match ready!(self.poll_fill(cx, 8 * 1024)) {
				Ok(Some(bytes)) => {
					self.queued_len += bytes.len();
					self.queued.push_back(bytes);
				}
				Ok(None) => {}
				// A read error is also a closed stream.
				Err(err) => return Poll::Ready(Err(err)),
			}
		}
	}
}

impl<S: web_transport_trait::RecvStream + 'static> Drop for AsyncRecv<S> {
	fn drop(&mut self) {
		match self.state.get_mut().unwrap().take() {
			// Apply a deferred stop if the stream is idle.
			Some(RecvState::Idle(mut stream)) => {
				if let Some(code) = self.stop.take() {
					stream.stop(code);
				}
			}
			// The stream lives inside the in-flight read; dropping it here would
			// fire the backend's default drop behavior (a code-0 stop) and lose
			// the deferred code the peer classifies the abort by. Finish the
			// read on the runtime and stop with the real code then; a deferred
			// stop fired the interrupt when it was recorded, so the task never
			// waits on blocked I/O. Without a runtime the default drop stands.
			Some(RecvState::Reading(fut)) => {
				if let Some(code) = self.stop.take()
					&& let Ok(handle) = tokio::runtime::Handle::try_current()
				{
					handle.spawn(async move {
						let (mut stream, _) = fut.await;
						stream.stop(code);
					});
				}
			}
			None => {}
		}
	}
}

#[cfg(test)]
mod tests {
	use std::sync::{
		Arc, Mutex,
		atomic::{AtomicBool, Ordering},
	};

	use super::*;
	use web_transport_trait::poll::{RecvStream as _, SendStream as _};

	#[derive(Debug, Clone, Default, PartialEq)]
	struct FakeError;

	impl std::fmt::Display for FakeError {
		fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
			write!(f, "fake error")
		}
	}

	impl std::error::Error for FakeError {}

	impl web_transport_trait::Error for FakeError {
		fn session_error(&self) -> Option<(u32, String)> {
			None
		}
	}

	/// An async-interface send stream whose writes can be blocked, so a test can
	/// hold an operation in flight inside the adapter.
	#[derive(Clone, Default)]
	struct FakeSend {
		writes: Arc<Mutex<Vec<u8>>>,
		blocked: Arc<AtomicBool>,
		priorities: Arc<Mutex<Vec<u8>>>,
		finished: Arc<AtomicBool>,
		resets: Arc<Mutex<Vec<u32>>>,
		/// The peer never acknowledges the FIN: closed() stays pending forever.
		never_ack: Arc<AtomicBool>,
	}

	impl web_transport_trait::SendStream for FakeSend {
		type Error = FakeError;

		async fn write(&mut self, buf: &[u8]) -> Result<usize, Self::Error> {
			std::future::poll_fn(|cx| {
				if self.blocked.load(Ordering::SeqCst) {
					cx.waker().wake_by_ref();
					return Poll::Pending;
				}
				Poll::Ready(())
			})
			.await;
			self.writes.lock().unwrap().extend_from_slice(buf);
			Ok(buf.len())
		}

		fn set_priority(&mut self, order: u8) {
			self.priorities.lock().unwrap().push(order);
		}

		fn finish(&mut self) -> Result<(), Self::Error> {
			self.finished.store(true, Ordering::SeqCst);
			Ok(())
		}

		fn reset(&mut self, code: u32) {
			self.resets.lock().unwrap().push(code);
		}

		async fn closed(&mut self) -> Result<(), Self::Error> {
			match self.finished.load(Ordering::SeqCst) && !self.never_ack.load(Ordering::SeqCst) {
				true => Ok(()),
				false => std::future::pending().await,
			}
		}
	}

	/// An async-interface receive stream replaying canned chunks, then EOF.
	struct FakeRecv {
		chunks: std::collections::VecDeque<Bytes>,
		stops: Arc<Mutex<Vec<u32>>>,
		/// Reads park while set, so a test can hold one in flight in the adapter.
		blocked: Arc<AtomicBool>,
	}

	impl web_transport_trait::RecvStream for FakeRecv {
		type Error = FakeError;

		async fn read(&mut self, dst: &mut [u8]) -> Result<Option<usize>, Self::Error> {
			std::future::poll_fn(|cx| {
				if self.blocked.load(Ordering::SeqCst) {
					cx.waker().wake_by_ref();
					return Poll::Pending;
				}
				Poll::Ready(())
			})
			.await;
			let Some(chunk) = self.chunks.front_mut() else {
				return Ok(None);
			};
			let n = dst.len().min(chunk.len());
			dst[..n].copy_from_slice(&chunk.split_to(n));
			if chunk.is_empty() {
				self.chunks.pop_front();
			}
			Ok(Some(n))
		}

		fn stop(&mut self, code: u32) {
			self.stops.lock().unwrap().push(code);
		}

		async fn closed(&mut self) -> Result<(), Self::Error> {
			Ok(())
		}
	}

	fn cx() -> Context<'static> {
		Context::from_waker(std::task::Waker::noop())
	}

	// The regression behind goaway_gates_new_subscribes_moq_lite_04: a dropped
	// closed() watch must not hold the stream hostage, so a later write still
	// goes through.
	#[test]
	fn send_closed_watch_does_not_block_writes() {
		let fake = FakeSend::default();
		let mut send = AsyncSend::new(fake.clone());
		let mut cx = cx();

		// A pre-terminal closed watch stays pending without consuming the stream.
		assert!(send.poll_closed(&mut cx).is_pending());

		// The write proceeds; the old ownership-transfer watch deadlocked here.
		assert_eq!(send.poll_write(&mut cx, b"hello"), Poll::Ready(Ok(5)));
		assert_eq!(fake.writes.lock().unwrap().as_slice(), b"hello");
	}

	// After finish() the real closed() watch runs and resolves.
	#[test]
	fn send_closed_resolves_after_finish() {
		let fake = FakeSend::default();
		let mut send = AsyncSend::new(fake.clone());
		let mut cx = cx();

		assert_eq!(send.poll_write(&mut cx, b"bye"), Poll::Ready(Ok(3)));
		send.finish().unwrap();
		assert_eq!(send.poll_closed(&mut cx), Poll::Ready(Ok(())));
	}

	// Actions issued while a write is in flight apply when it settles, and the
	// settled write reports its original length.
	#[test]
	fn send_defers_actions_across_an_inflight_write() {
		let fake = FakeSend::default();
		fake.blocked.store(true, Ordering::SeqCst);
		let mut send = AsyncSend::new(fake.clone());
		let mut cx = cx();

		assert!(send.poll_write(&mut cx, b"queued").is_pending());
		send.set_priority(7);
		assert!(fake.priorities.lock().unwrap().is_empty(), "deferred while in flight");

		fake.blocked.store(false, Ordering::SeqCst);
		assert_eq!(send.poll_write(&mut cx, b"queued"), Poll::Ready(Ok(6)));
		assert_eq!(fake.priorities.lock().unwrap().as_slice(), &[7]);
	}

	// The cluster-failover corruption: a write held Pending by backpressure is
	// driven to completion by the peer-close check (poll_closed). Its result must
	// reach the retrying poll_write; discarding it made the retry put the same
	// bytes on the wire twice, which a peer decodes as a phantom frame.
	#[test]
	fn a_write_completed_by_poll_closed_is_not_duplicated() {
		let fake = FakeSend::default();
		fake.blocked.store(true, Ordering::SeqCst);
		let mut send = AsyncSend::new(fake.clone());
		let mut cx = cx();

		// Backpressure: the write parks inside the adapter.
		assert!(send.poll_write(&mut cx, b"header").is_pending());

		// The pressure lifts; the driver's next pass checks for a peer close
		// first, which drives the stored write to completion.
		fake.blocked.store(false, Ordering::SeqCst);
		assert!(send.poll_closed(&mut cx).is_pending(), "not terminal yet");

		// The retry reports the completed write instead of writing again.
		assert_eq!(send.poll_write(&mut cx, b"header"), Poll::Ready(Ok(6)));
		assert_eq!(
			fake.writes.lock().unwrap().as_slice(),
			b"header",
			"the bytes must reach the stream exactly once"
		);
	}

	// A late reset must not wait behind a FIN acknowledgement that never comes:
	// the group machines finish a stream and keep watching precisely so a late
	// cancel can still reset it.
	#[test]
	fn a_late_reset_interrupts_the_ack_watch() {
		let fake = FakeSend::default();
		fake.never_ack.store(true, Ordering::SeqCst);
		let mut send = AsyncSend::new(fake.clone());
		let mut cx = cx();

		assert_eq!(send.poll_write(&mut cx, b"bye"), Poll::Ready(Ok(3)));
		send.finish().unwrap();
		// The ack never arrives; the watch parks.
		assert!(send.poll_closed(&mut cx).is_pending());

		// The late cancel: the reset applies without waiting for the ack, and
		// the watch then resolves against the reset stream.
		send.reset(9);
		let closed = send.poll_closed(&mut cx);
		assert_eq!(fake.resets.lock().unwrap().as_slice(), &[9]);
		// The re-armed watch on the reset stream is allowed to stay pending in
		// this fake (it never acks); what matters is the reset went out.
		let _ = closed;
	}

	// The poll contract allows retrying a pending write with a SHORTER buffer;
	// transmitted-but-unreported bytes reconcile against the buffer presented,
	// never reporting more than its length and never re-transmitting.
	#[test]
	fn a_completed_write_reconciles_against_a_shorter_retry() {
		let fake = FakeSend::default();
		fake.blocked.store(true, Ordering::SeqCst);
		let mut send = AsyncSend::new(fake.clone());
		let mut cx = cx();

		assert!(send.poll_write(&mut cx, b"header").is_pending());
		fake.blocked.store(false, Ordering::SeqCst);
		assert!(send.poll_closed(&mut cx).is_pending());

		// The caller shrank its buffer: only that much is reported per call.
		assert_eq!(send.poll_write(&mut cx, b"hea"), Poll::Ready(Ok(3)));
		assert_eq!(send.poll_write(&mut cx, b"der"), Poll::Ready(Ok(3)));
		assert_eq!(
			fake.writes.lock().unwrap().as_slice(),
			b"header",
			"the bytes must reach the stream exactly once"
		);

		// Fully reconciled: the next write is a fresh operation.
		assert_eq!(send.poll_write(&mut cx, b"!"), Poll::Ready(Ok(1)));
		assert_eq!(fake.writes.lock().unwrap().as_slice(), b"header!");
	}

	// A larger chunk than the destination is buffered and served across reads.
	#[test]
	fn recv_buffers_the_excess() {
		let stops = Arc::new(Mutex::new(Vec::new()));
		let fake = FakeRecv {
			chunks: [Bytes::from_static(b"abcdef")].into_iter().collect(),
			stops: stops.clone(),
			blocked: Default::default(),
		};
		let mut recv = AsyncRecv::new(fake);
		let mut cx = cx();

		let mut dst = [0u8; 4];
		assert_eq!(recv.poll_read(&mut cx, &mut dst), Poll::Ready(Ok(Some(4))));
		assert_eq!(&dst, b"abcd");

		// The retry may shrink; the buffered tail serves it.
		let mut dst = [0u8; 2];
		assert_eq!(recv.poll_read(&mut cx, &mut dst), Poll::Ready(Ok(Some(2))));
		assert_eq!(&dst, b"ef");

		// EOF, and the closed watch resolves once drained.
		assert_eq!(recv.poll_read(&mut cx, &mut dst), Poll::Ready(Ok(None)));
		assert_eq!(recv.poll_closed(&mut cx), Poll::Ready(Ok(())));
	}

	// stop() while idle applies immediately.
	#[test]
	fn recv_stop_applies_when_idle() {
		let stops = Arc::new(Mutex::new(Vec::new()));
		let fake = FakeRecv {
			chunks: Default::default(),
			stops: stops.clone(),
			blocked: Default::default(),
		};
		let mut recv = AsyncRecv::new(fake);

		recv.stop(42);
		assert_eq!(stops.lock().unwrap().as_slice(), &[42]);
	}

	// A stop issued while a read is in flight goes out when that read settles,
	// not on a later read that may never come.
	#[test]
	fn recv_stop_applies_when_the_read_settles() {
		let stops = Arc::new(Mutex::new(Vec::new()));
		let blocked = Arc::new(AtomicBool::new(true));
		let fake = FakeRecv {
			chunks: [Bytes::from_static(b"data")].into_iter().collect(),
			stops: stops.clone(),
			blocked: blocked.clone(),
		};
		let mut recv = AsyncRecv::new(fake);
		let mut cx = cx();

		let mut dst = [0u8; 4];
		assert!(recv.poll_read(&mut cx, &mut dst).is_pending());
		recv.stop(7);
		assert!(stops.lock().unwrap().is_empty(), "deferred while in flight");

		// The read settles (here via a retry); the stop goes out with it.
		blocked.store(false, Ordering::SeqCst);
		assert_eq!(recv.poll_read(&mut cx, &mut dst), Poll::Ready(Ok(Some(4))));
		assert_eq!(stops.lock().unwrap().as_slice(), &[7]);
	}

	// A writer dropped while its write is still in flight must not lose the
	// deferred reset code: the peer classifies the abort by it (Old vs Cancel),
	// so the salvage task finishes the write and applies the real code.
	#[tokio::test]
	async fn a_deferred_reset_code_survives_a_drop_mid_write() {
		let fake = FakeSend::default();
		fake.blocked.store(true, Ordering::SeqCst);
		let mut send = AsyncSend::new(fake.clone());
		let mut cx = cx();

		assert!(send.poll_write(&mut cx, b"payload").is_pending());
		send.reset(9);
		drop(send);

		// The write stays blocked forever: the reset must not wait behind it.
		tokio::time::timeout(std::time::Duration::from_secs(1), async {
			while fake.resets.lock().unwrap().is_empty() {
				tokio::task::yield_now().await;
			}
		})
		.await
		.expect("the salvage task never applied the reset");
		assert_eq!(fake.resets.lock().unwrap().as_slice(), &[9]);
	}

	// The receive-side twin: a reader dropped mid-read still sends the deferred
	// STOP_SENDING with the real code.
	#[tokio::test]
	async fn a_deferred_stop_code_survives_a_drop_mid_read() {
		let stops = Arc::new(Mutex::new(Vec::new()));
		let blocked = Arc::new(AtomicBool::new(true));
		let fake = FakeRecv {
			chunks: [Bytes::from_static(b"data")].into_iter().collect(),
			stops: stops.clone(),
			blocked: blocked.clone(),
		};
		let mut recv = AsyncRecv::new(fake);
		let mut cx = cx();

		let mut dst = [0u8; 4];
		assert!(recv.poll_read(&mut cx, &mut dst).is_pending());
		wt_poll::RecvStream::stop(&mut recv, 11);
		drop(recv);

		// The read stays blocked forever: the stop must not wait behind it.
		tokio::time::timeout(std::time::Duration::from_secs(1), async {
			while stops.lock().unwrap().is_empty() {
				tokio::task::yield_now().await;
			}
		})
		.await
		.expect("the salvage task never applied the stop");
		assert_eq!(stops.lock().unwrap().as_slice(), &[11]);
	}

	// A FIN sitting behind undelivered payload resolves the closed watch without
	// an external read; the payload is still delivered afterward, in order.
	#[test]
	fn recv_closed_resolves_behind_buffered_data() {
		let stops = Arc::new(Mutex::new(Vec::new()));
		let fake = FakeRecv {
			chunks: [Bytes::from_static(b"tail")].into_iter().collect(),
			stops: stops.clone(),
			blocked: Default::default(),
		};
		let mut recv = AsyncRecv::new(fake);
		let mut cx = cx();

		// The watch reads through the payload to the FIN.
		assert_eq!(recv.poll_closed(&mut cx), Poll::Ready(Ok(())));

		// Nothing was lost: the read-ahead serves the payload, then EOF.
		let mut dst = [0u8; 4];
		assert_eq!(recv.poll_read(&mut cx, &mut dst), Poll::Ready(Ok(Some(4))));
		assert_eq!(&dst, b"tail");
		assert_eq!(recv.poll_read(&mut cx, &mut dst), Poll::Ready(Ok(None)));
	}
}
