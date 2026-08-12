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

use std::task::{Context, Poll, ready};

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

/// A connected browser WebTransport session, usable by `moq-net`.
pub struct Session {
	inner: web_transport_wasm::Session,
	accept_uni: OpSlot<Result<RecvStream, Error>>,
	accept_bi: OpSlot<Result<(SendStream, RecvStream), Error>>,
	open_uni: OpSlot<Result<SendStream, Error>>,
	open_bi: OpSlot<Result<(SendStream, RecvStream), Error>>,
	recv_datagram: OpSlot<Result<Bytes, Error>>,
	closed: OpSlot<Error>,
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
		}
	}
}

// Manual impl: a clone starts with no in-flight operations, since each handle
// owns its own progress under the poll contract.
impl Clone for Session {
	fn clone(&self) -> Self {
		Self::new(self.inner.clone())
	}
}

/// Open a browser WebTransport connection to `url`.
pub async fn connect(url: Url) -> Result<Session, Error> {
	let client = web_transport_wasm::ClientBuilder::new().with_system_roots();
	let session = client.connect(url).await.map_err(Error)?;
	Ok(Session::new(session))
}

/// Connect, trusting only the given sha-256 certificate hashes (serverless dev,
/// matching the browser's `serverCertificateHashes` option).
pub async fn connect_with_hashes(url: Url, hashes: Vec<Vec<u8>>) -> Result<Session, Error> {
	let client = web_transport_wasm::ClientBuilder::new().with_server_certificate_hashes(hashes);
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
		// The browser datagram API is async. moq drives all control/media over
		// streams, so fire-and-forget is acceptable here.
		let session = self.inner.clone();
		let payload = Bytes::copy_from_slice(payload);
		web_async::spawn(async move {
			let _ = session.send_datagram(payload).await;
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
	/// A write driven to completion by `poll_closed` rather than `poll_write`. The
	/// caller was told `Pending` and is contractually retrying the same bytes, so the
	/// next `poll_write` reports this length instead of writing them a second time.
	completed: Option<usize>,
}

enum SendState {
	Idle(web_transport_wasm::SendStream),
	/// A write in flight; `len` is what to report when it resolves.
	Writing {
		fut: LocalBoxFuture<'static, (web_transport_wasm::SendStream, Result<(), Error>)>,
		len: usize,
	},
	Closing(LocalBoxFuture<'static, (web_transport_wasm::SendStream, Result<(), Error>)>),
}

impl SendStream {
	fn new(stream: web_transport_wasm::SendStream) -> Self {
		Self {
			state: Some(SendState::Idle(stream)),
			priority: None,
			finish: false,
			reset: None,
			terminal: false,
			completed: None,
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
		// A previous write completed inside poll_closed; report it instead of
		// writing the same bytes a second time.
		if let Some(len) = self.completed.take() {
			return Poll::Ready(Ok(len));
		}
		loop {
			match self.state.take().expect("in-flight") {
				SendState::Idle(mut stream) => {
					if buf.is_empty() {
						self.state = Some(SendState::Idle(stream));
						return Poll::Ready(Ok(0));
					}
					// The wasm writer writes the whole slice or errors, so the
					// reported count is exact. The caller retries with the same
					// bytes until this resolves.
					let chunk = buf.to_vec();
					let len = chunk.len();
					let fut = async move {
						let res = stream.write(&chunk).await.map_err(Error);
						(stream, res)
					}
					.boxed_local();
					self.state = Some(SendState::Writing { fut, len });
				}
				SendState::Writing { mut fut, len } => match fut.as_mut().poll(cx) {
					Poll::Pending => {
						self.state = Some(SendState::Writing { fut, len });
						return Poll::Pending;
					}
					Poll::Ready((mut stream, res)) => {
						self.settle(&mut stream);
						self.state = Some(SendState::Idle(stream));
						res?;
						return Poll::Ready(Ok(len));
					}
				},
				SendState::Closing(mut fut) => match fut.as_mut().poll(cx) {
					Poll::Pending => {
						self.state = Some(SendState::Closing(fut));
						return Poll::Pending;
					}
					Poll::Ready((mut stream, _res)) => {
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
			_ => self.reset = Some(code),
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
					let fut = async move {
						let res = stream.closed().await.map(|_| ()).map_err(Error);
						(stream, res)
					}
					.boxed_local();
					self.state = Some(SendState::Closing(fut));
				}
				SendState::Writing { mut fut, len } => match fut.as_mut().poll(cx) {
					Poll::Pending => {
						self.state = Some(SendState::Writing { fut, len });
						return Poll::Pending;
					}
					Poll::Ready((mut stream, res)) => {
						self.settle(&mut stream);
						self.state = Some(SendState::Idle(stream));
						// A failed write is a closed stream; report it here.
						if let Err(err) = res {
							return Poll::Ready(Err(err));
						}
						// The caller of poll_write was told Pending and will retry the
						// same bytes; record the completion so the retry reports it
						// instead of writing the bytes a second time.
						self.completed = Some(len);
					}
				},
				SendState::Closing(mut fut) => match fut.as_mut().poll(cx) {
					Poll::Pending => {
						self.state = Some(SendState::Closing(fut));
						return Poll::Pending;
					}
					Poll::Ready((mut stream, res)) => {
						self.settle(&mut stream);
						self.state = Some(SendState::Idle(stream));
						return Poll::Ready(res);
					}
				},
			}
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
}

/// How much the closed-watch may read ahead of the caller before it parks and
/// waits for the caller to drain. Bounds memory against a peer that floods a
/// stream nobody is reading.
const READ_AHEAD_CAP: usize = 64 * 1024;

enum RecvState {
	Idle(web_transport_wasm::RecvStream),
	Reading(LocalBoxFuture<'static, (web_transport_wasm::RecvStream, Result<Option<Bytes>, Error>)>),
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
					let fut = async move {
						let res = stream.read(max).await.map_err(Error);
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
						// Apply a stop requested while the read was in flight now, not on
						// the next read: a caller that stops and never reads again must
						// still send STOP_SENDING.
						if let Some(code) = self.stop.take() {
							stream.stop(&code.to_string());
						}
						self.state = Some(RecvState::Idle(stream));
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
			_ => self.stop = Some(code),
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
