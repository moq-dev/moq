//! Adapt `web-transport-wasm` (the browser WebTransport API) to the poll
//! interface (`web_transport_trait::poll`) that `moq-net` requires.
//!
//! The browser API is promise-based, so each pending operation is stored as a
//! boxed local future: session operations clone the underlying handle, stream
//! operations move the stream into the future and take it back when the
//! operation settles. Stream closed-watches are emulated (reads report the FIN
//! for receive streams; send streams only start the real watch after
//! `finish`/`reset`), since the underlying `closed()` future would own the
//! stream and deadlock any later read or write. This mirrors what a native
//! poll implementation inside `web-transport-wasm` would do better; migrate
//! there once it grows one.

use std::{
	cell::Cell,
	rc::Rc,
	task::{Context, Poll, ready},
};

use bytes::Bytes;
use futures::{FutureExt, future::LocalBoxFuture};
use url::Url;
use web_transport_trait as wtt;

/// A stored in-flight operation: `None` when idle. The browser is single
/// threaded, so the plain local flavor suffices.
type OpSlot<T> = Option<LocalBoxFuture<'static, T>>;

/// Lazily start and poll the operation in `slot`, clearing it when it resolves.
fn poll_op<T>(
	slot: &mut OpSlot<T>,
	cx: &mut Context<'_>,
	start: impl FnOnce() -> LocalBoxFuture<'static, T>,
) -> Poll<T> {
	let fut = slot.get_or_insert_with(start);
	let output = ready!(fut.as_mut().poll(cx));
	*slot = None;
	Poll::Ready(output)
}

/// How many browser datagram sends may be outstanding before further ones are
/// dropped. The browser API is promise-based with no readiness signal, so this
/// is what keeps a backpressured connection from accumulating tasks and payload
/// copies: at the 1200 byte limit below, the ceiling is under 10 KiB in flight.
const MAX_PENDING_DATAGRAMS: usize = 8;

/// A connected browser WebTransport session, usable by `moq-net`.
pub struct Session {
	inner: web_transport_wasm::Session,
	accept_uni: OpSlot<Result<RecvStream, Error>>,
	accept_bi: OpSlot<Result<(SendStream, RecvStream), Error>>,
	open_uni: OpSlot<Result<SendStream, Error>>,
	open_bi: OpSlot<Result<(SendStream, RecvStream), Error>>,
	recv_datagram: OpSlot<Result<Bytes, Error>>,
	closed: OpSlot<Error>,
	/// Datagram sends spawned but not yet settled. Shared by every clone, since
	/// the connection they contend for is the shared resource.
	datagrams_pending: Rc<Cell<usize>>,
}

impl Session {
	fn new(inner: web_transport_wasm::Session) -> Self {
		Self {
			inner,
			accept_uni: None,
			accept_bi: None,
			open_uni: None,
			open_bi: None,
			recv_datagram: None,
			closed: None,
			datagrams_pending: Rc::new(Cell::new(0)),
		}
	}
}

// Manual impl: a clone starts with no in-flight operations, since each handle
// owns its own progress under the poll contract. The datagram budget is the
// exception, being a property of the connection rather than of one handle.
impl Clone for Session {
	fn clone(&self) -> Self {
		Self {
			datagrams_pending: self.datagrams_pending.clone(),
			..Self::new(self.inner.clone())
		}
	}
}

/// Options for a browser WebTransport connection.
///
/// Build via [`Default`] and set fields; new knobs are added here rather than
/// as new `connect` parameters.
#[derive(Default)]
#[non_exhaustive]
pub struct Options {
	/// Trust only these sha-256 certificate hashes instead of the system roots
	/// (serverless dev, matching the browser's `serverCertificateHashes`).
	pub server_certificate_hashes: Vec<Vec<u8>>,
}

/// Open a browser WebTransport connection to `url`.
pub async fn connect(url: Url, options: Options) -> Result<Session, Error> {
	let client = web_transport_wasm::ClientBuilder::new();
	let client = match options.server_certificate_hashes.is_empty() {
		true => client.with_system_roots(),
		false => client.with_server_certificate_hashes(options.server_certificate_hashes),
	};
	let session = client.connect(url).await.map_err(Error)?;
	Ok(Session::new(session))
}

/// Wraps `web_transport_wasm::Error` so we can implement the foreign error trait.
#[derive(Debug, Clone, thiserror::Error)]
#[error(transparent)]
pub struct Error(web_transport_wasm::Error);

impl wtt::Error for Error {
	// The browser reports one code either way, so the variant is the only thing saying
	// which registry it belongs to. Answering both would have a stream reset decoded
	// against the session table, since callers ask about the session first.
	fn session_error(&self) -> Option<(u32, String)> {
		match self.0 {
			web_transport_wasm::Error::Session(_) => self.0.code().map(|code| (code as u32, self.0.to_string())),
			_ => None,
		}
	}

	fn stream_error(&self) -> Option<u32> {
		match self.0 {
			web_transport_wasm::Error::Stream(_) => self.0.code().map(|c| c as u32),
			_ => None,
		}
	}
}

impl wtt::poll::Session for Session {
	type SendStream = SendStream;
	type RecvStream = RecvStream;
	type Error = Error;

	fn poll_accept_uni(&mut self, cx: &mut Context<'_>) -> Poll<Result<Self::RecvStream, Self::Error>> {
		let inner = &self.inner;
		poll_op(&mut self.accept_uni, cx, || {
			let session = inner.clone();
			async move { Ok(RecvStream::new(session.accept_uni().await.map_err(Error)?)) }.boxed_local()
		})
	}

	fn poll_accept_bi(&mut self, cx: &mut Context<'_>) -> Poll<Result<(SendStream, RecvStream), Self::Error>> {
		let inner = &self.inner;
		poll_op(&mut self.accept_bi, cx, || {
			let session = inner.clone();
			async move {
				let (s, r) = session.accept_bi().await.map_err(Error)?;
				Ok((SendStream::new(s), RecvStream::new(r)))
			}
			.boxed_local()
		})
	}

	fn poll_open_bi(&mut self, cx: &mut Context<'_>) -> Poll<Result<(SendStream, RecvStream), Self::Error>> {
		let inner = &self.inner;
		poll_op(&mut self.open_bi, cx, || {
			let session = inner.clone();
			async move {
				let (s, r) = session.open_bi().await.map_err(Error)?;
				Ok((SendStream::new(s), RecvStream::new(r)))
			}
			.boxed_local()
		})
	}

	fn poll_open_uni(&mut self, cx: &mut Context<'_>) -> Poll<Result<Self::SendStream, Self::Error>> {
		let inner = &self.inner;
		poll_op(&mut self.open_uni, cx, || {
			let session = inner.clone();
			async move { Ok(SendStream::new(session.open_uni().await.map_err(Error)?)) }.boxed_local()
		})
	}

	fn poll_send_datagram(&mut self, _cx: &mut Context<'_>, payload: &[u8]) -> Poll<Result<(), Self::Error>> {
		// The browser datagram API is async, so the send runs on its own task and
		// the caller is told the datagram was accepted either way: datagrams are
		// best-effort, and the publisher has no fallback to offer if we said no.
		// What it must not do is let a producer outrunning the network queue those
		// tasks without bound, so past the budget the datagram is dropped here,
		// which is the congestion behavior the caller already expects.
		let pending = self.datagrams_pending.get();
		if pending >= MAX_PENDING_DATAGRAMS {
			return Poll::Ready(Ok(()));
		}
		self.datagrams_pending.set(pending + 1);

		let session = self.inner.clone();
		let payload = Bytes::copy_from_slice(payload);
		let budget = self.datagrams_pending.clone();
		web_async::spawn(async move {
			let _ = session.send_datagram(payload).await;
			budget.set(budget.get() - 1);
		});
		Poll::Ready(Ok(()))
	}

	fn poll_recv_datagram(&mut self, cx: &mut Context<'_>) -> Poll<Result<Bytes, Self::Error>> {
		let inner = &self.inner;
		poll_op(&mut self.recv_datagram, cx, || {
			let session = inner.clone();
			async move { session.recv_datagram().await.map_err(Error) }.boxed_local()
		})
	}

	fn max_datagram_size(&self) -> usize {
		// The browser doesn't expose this; use the conservative QUIC default.
		1200
	}

	fn protocol(&self) -> Option<&str> {
		self.inner.protocol()
	}

	fn close(&mut self, code: u32, reason: &str) {
		self.inner.close(code, reason);
	}

	fn poll_closed(&mut self, cx: &mut Context<'_>) -> Poll<Self::Error> {
		let inner = &self.inner;
		poll_op(&mut self.closed, cx, || {
			let session = inner.clone();
			async move { Error(session.closed().await) }.boxed_local()
		})
	}

	fn stats(&self) -> impl wtt::Stats {
		wtt::StatsUnavailable
	}
}

/// The send half of a browser stream, adapted to the poll interface.
pub struct SendStream {
	state: Option<SendState>,
	// Deferred actions, applied when the in-flight operation settles.
	priority: Option<i32>,
	finish: bool,
	reset: Option<u32>,
	/// Whether `finish` or `reset` has been observed, so no further writes can
	/// come and the closed() watch may safely take ownership of the stream.
	terminal: bool,
	/// Bytes already transmitted but not yet reported to the caller: a write the
	/// stored future finished (possibly inside `poll_closed`) while the caller
	/// held a `Pending`. Later `poll_write` calls reconcile against the buffer
	/// actually presented (the poll contract allows a shorter retry), reporting
	/// and consuming a prefix per call, never writing these bytes a second time.
	completed: Bytes,
	/// Cancels the in-flight write so a reset applies immediately instead of
	/// waiting behind blocked I/O (whose partial progress a reset discards
	/// anyway). Fired by `reset` and by `Drop`.
	interrupt: Option<futures::channel::oneshot::Sender<()>>,
}

enum SendState {
	Idle(web_transport_wasm::SendStream),
	/// A write in flight; `chunk` is the copied bytes, kept (refcounted, no
	/// extra copy) so a completion absorbed by `poll_closed` can verify the
	/// retrying caller supplied the same bytes. A `None` result means the write
	/// was interrupted by a reset (see `SendStream::interrupt`).
	Writing {
		#[allow(clippy::type_complexity)]
		fut: LocalBoxFuture<'static, (web_transport_wasm::SendStream, Option<Result<(), Error>>)>,
		chunk: Bytes,
	},
	/// The closed() acknowledgement watch; a `None` result means it was
	/// interrupted by a late reset (see `SendStream::interrupt`).
	Closing(
		#[allow(clippy::type_complexity)]
		LocalBoxFuture<'static, (web_transport_wasm::SendStream, Option<Result<(), Error>>)>,
	),
}

impl SendStream {
	fn new(stream: web_transport_wasm::SendStream) -> Self {
		Self {
			state: Some(SendState::Idle(stream)),
			priority: None,
			finish: false,
			reset: None,
			terminal: false,
			completed: Bytes::new(),
			interrupt: None,
		}
	}

	/// Apply actions deferred while an operation was in flight.
	fn settle(&mut self, stream: &mut web_transport_wasm::SendStream) {
		if let Some(order) = self.priority.take() {
			stream.set_priority(order);
		}
		if let Some(code) = self.reset.take() {
			self.finish = false;
			stream.reset(&code.to_string());
		}
		if std::mem::take(&mut self.finish) {
			// A deferred finish has nowhere to report to; its failure surfaces
			// through closed() like any other terminal state.
			let _ = stream.finish();
		}
	}
}

impl wtt::poll::SendStream for SendStream {
	type Error = Error;

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
			match self.state.take().expect("in-flight") {
				SendState::Idle(mut stream) => {
					if buf.is_empty() {
						self.state = Some(SendState::Idle(stream));
						return Poll::Ready(Ok(0));
					}
					// The wasm writer writes the whole slice or errors, so the
					// reported count is exact. The caller retries with the same
					// bytes until this resolves.
					let chunk = Bytes::copy_from_slice(buf);
					let retained = chunk.clone();
					let (tx, rx) = futures::channel::oneshot::channel::<()>();
					self.interrupt = Some(tx);
					let fut = async move {
						let res = {
							let mut op = std::pin::pin!(async { stream.write(&chunk).await.map_err(Error) }.fuse());
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
					.boxed_local();
					self.state = Some(SendState::Writing { fut, chunk: retained });
				}
				SendState::Writing { mut fut, chunk } => match fut.as_mut().poll(cx) {
					Poll::Pending => {
						self.state = Some(SendState::Writing { fut, chunk });
						return Poll::Pending;
					}
					Poll::Ready((mut stream, res)) => {
						self.interrupt = None;
						self.settle(&mut stream);
						self.state = Some(SendState::Idle(stream));
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
						self.state = Some(SendState::Closing(fut));
						return Poll::Pending;
					}
					Poll::Ready((mut stream, _res)) => {
						self.interrupt = None;
						self.settle(&mut stream);
						self.state = Some(SendState::Idle(stream));
					}
				},
			}
		}
	}

	fn set_priority(&mut self, order: u8) {
		match self.state.as_mut() {
			Some(SendState::Idle(stream)) => stream.set_priority(order as i32),
			_ => self.priority = Some(order as i32),
		}
	}

	fn finish(&mut self) -> Result<(), Self::Error> {
		self.terminal = true;
		match self.state.as_mut() {
			Some(SendState::Idle(stream)) => stream.finish().map_err(Error),
			_ => {
				self.finish = true;
				Ok(())
			}
		}
	}

	fn reset(&mut self, code: u32) {
		self.terminal = true;
		match self.state.as_mut() {
			Some(SendState::Idle(stream)) => stream.reset(&code.to_string()),
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
			match self.state.take().expect("in-flight") {
				SendState::Idle(mut stream) => {
					self.settle(&mut stream);
					// Only a finished (or reset) stream starts the real closed()
					// watch: that future owns the stream, and a stream the caller
					// may still write to must stay reclaimable. Before that,
					// closure surfaces as an error on the next operation instead.
					if !self.terminal {
						self.state = Some(SendState::Idle(stream));
						return Poll::Pending;
					}
					// Interruptible like a write: a late reset (the group machines
					// cancel a finished stream this way) must not wait for a FIN
					// acknowledgement that may never come.
					let (tx, rx) = futures::channel::oneshot::channel::<()>();
					self.interrupt = Some(tx);
					let fut = async move {
						let res = {
							let mut op =
								std::pin::pin!(async { stream.closed().await.map(|_| ()).map_err(Error) }.fuse());
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
					.boxed_local();
					self.state = Some(SendState::Closing(fut));
				}
				SendState::Writing { mut fut, chunk } => match fut.as_mut().poll(cx) {
					Poll::Pending => {
						self.state = Some(SendState::Writing { fut, chunk });
						return Poll::Pending;
					}
					Poll::Ready((mut stream, res)) => {
						self.interrupt = None;
						self.settle(&mut stream);
						self.state = Some(SendState::Idle(stream));
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
						self.state = Some(SendState::Closing(fut));
						return Poll::Pending;
					}
					Poll::Ready((mut stream, res)) => {
						self.interrupt = None;
						self.settle(&mut stream);
						self.state = Some(SendState::Idle(stream));
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

impl Drop for SendStream {
	fn drop(&mut self) {
		match self.state.take() {
			// Apply a deferred reset if the stream is idle.
			Some(SendState::Idle(mut stream)) => {
				if let Some(code) = self.reset.take() {
					stream.reset(&code.to_string());
				}
			}
			// The stream lives inside the in-flight future; dropping it here
			// would lose the deferred reset code or FIN. The reset already fired
			// the interrupt, so the future resolves promptly; finish it on a
			// local task and apply the terminal action then.
			Some(SendState::Writing { fut, .. }) => {
				let reset = self.reset.take();
				let finish = std::mem::take(&mut self.finish);
				if reset.is_some() || finish {
					web_async::spawn(async move {
						let (mut stream, _) = fut.await;
						match reset {
							Some(code) => stream.reset(&code.to_string()),
							None => {
								let _ = stream.finish();
							}
						}
					});
				}
			}
			Some(SendState::Closing(fut)) => {
				let reset = self.reset.take();
				if let Some(code) = reset {
					web_async::spawn(async move {
						let (mut stream, _) = fut.await;
						stream.reset(&code.to_string());
					});
				}
			}
			None => {}
		}
	}
}

/// The receive half of a browser stream, adapted to the poll interface.
pub struct RecvStream {
	state: Option<RecvState>,
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
	/// waiting behind blocked I/O. Fired by `stop` and by `Drop`.
	interrupt: Option<futures::channel::oneshot::Sender<()>>,
}

/// How much the closed-watch may read ahead of the caller before it parks and
/// waits for the caller to drain. Bounds memory against a peer that floods a
/// stream nobody is reading.
const READ_AHEAD_CAP: usize = 64 * 1024;

enum RecvState {
	Idle(web_transport_wasm::RecvStream),
	/// A read in flight; a `None` result means it was interrupted by a stop
	/// (see `RecvStream::interrupt`).
	Reading(
		#[allow(clippy::type_complexity)]
		LocalBoxFuture<'static, (web_transport_wasm::RecvStream, Option<Result<Option<Bytes>, Error>>)>,
	),
}

impl RecvStream {
	fn new(stream: web_transport_wasm::RecvStream) -> Self {
		Self {
			state: Some(RecvState::Idle(stream)),
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
	fn poll_fill(&mut self, cx: &mut Context<'_>, max: usize) -> Poll<Result<Option<Bytes>, Error>> {
		loop {
			match self.state.take().expect("in-flight") {
				RecvState::Idle(mut stream) => {
					if let Some(code) = self.stop.take() {
						stream.stop(&code.to_string());
					}
					let (tx, rx) = futures::channel::oneshot::channel::<()>();
					self.interrupt = Some(tx);
					let fut = async move {
						let res = {
							let mut op = std::pin::pin!(async { stream.read(max).await.map_err(Error) }.fuse());
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
					.boxed_local();
					self.state = Some(RecvState::Reading(fut));
				}
				RecvState::Reading(mut fut) => match fut.as_mut().poll(cx) {
					Poll::Pending => {
						self.state = Some(RecvState::Reading(fut));
						return Poll::Pending;
					}
					Poll::Ready((mut stream, res)) => {
						self.interrupt = None;
						// Apply a stop requested while the read was in flight now, not on
						// the next read: a caller that stops and never reads again must
						// still send STOP_SENDING.
						if let Some(code) = self.stop.take() {
							stream.stop(&code.to_string());
						}
						self.state = Some(RecvState::Idle(stream));
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

impl wtt::poll::RecvStream for RecvStream {
	type Error = Error;

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
		match self.state.as_mut() {
			Some(RecvState::Idle(stream)) => stream.stop(&code.to_string()),
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
		// Emulated with reads: a FIN or reset surfaces through reads; payload
		// arriving before it is queued as read-ahead (up to READ_AHEAD_CAP) so a
		// FIN right behind undelivered data still resolves the watch without an
		// external read.
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

impl Drop for RecvStream {
	fn drop(&mut self) {
		match self.state.take() {
			// Apply a deferred stop if the stream is idle.
			Some(RecvState::Idle(mut stream)) => {
				if let Some(code) = self.stop.take() {
					stream.stop(&code.to_string());
				}
			}
			// The stream lives inside the in-flight read; the stop already fired
			// the interrupt, so the future resolves promptly. Finish it on a
			// local task and apply the real code then.
			Some(RecvState::Reading(fut)) => {
				if let Some(code) = self.stop.take() {
					web_async::spawn(async move {
						let (mut stream, _) = fut.await;
						stream.stop(&code.to_string());
					});
				}
			}
			None => {}
		}
	}
}
