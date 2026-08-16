use std::{
	collections::{HashMap, VecDeque},
	sync::{Arc, Mutex},
	task::{Context, Poll},
};

use bytes::{Buf, BufMut, Bytes, BytesMut};

use crate::{
	Error, PathOwned,
	coding::{Decode, Encode, Reader, Writer},
	ietf::{self, RequestId},
};

use super::{Control, Message, Version};

// === Message Queues ===

struct QueueState<T> {
	messages: VecDeque<T>,
	closed: bool,
}

/// An unbounded FIFO over [`kio::Shared`]: any clone can push or pop.
///
/// Closure is a plain flag rather than handle liveness: [`close`](Self::close)
/// (or dropping a [`QueueWriter`]) stops future pushes, and the reader drains
/// what's buffered before seeing `None`.
struct Queue<T>(kio::Shared<QueueState<T>>);

impl<T> Queue<T> {
	fn new() -> Self {
		Self(kio::Shared::new(QueueState {
			messages: VecDeque::new(),
			closed: false,
		}))
	}

	/// Enqueue a message. Returns false once the queue is closed (the message is
	/// dropped), matching a write to a stream whose reader is gone.
	fn push(&self, item: T) -> bool {
		let mut state = self.0.lock();
		if state.closed {
			return false;
		}
		state.messages.push_back(item);
		true
	}

	/// Dequeue the next message, waiting while the queue is open and empty.
	/// Returns `None` once the queue is closed and drained.
	async fn pop(&self) -> Option<T> {
		kio::wait(|waiter| self.poll_pop(waiter)).await
	}

	/// Poll for the next message; ready with `None` once closed and drained.
	fn poll_pop(&self, waiter: &kio::Waiter) -> Poll<Option<T>> {
		self.0
			.poll(waiter, |state| {
				if state.messages.is_empty() && !state.closed {
					Poll::Pending
				} else {
					Poll::Ready(())
				}
			})
			.map(|mut state| state.messages.pop_front())
	}

	/// Stop future pushes; buffered messages still drain, then `pop` returns `None`.
	fn close(&self) {
		self.0.lock().closed = true;
	}

	/// Mint the single owning push handle whose `Drop` closes the queue.
	fn writer(&self) -> QueueWriter<T> {
		QueueWriter(Self(self.0.clone()))
	}
}

// Manual impls: derives would demand `T: Clone`/`T: Default`, which the queued
// stream pairs don't satisfy.
impl<T> Clone for Queue<T> {
	fn clone(&self) -> Self {
		Self(self.0.clone())
	}
}

impl<T> Default for Queue<T> {
	fn default() -> Self {
		Self::new()
	}
}

/// Single-owner push handle: dropping it closes the queue, which the reading
/// [`VirtualRecvStream`] observes as a FIN after draining.
struct QueueWriter<T>(Queue<T>);

impl<T> QueueWriter<T> {
	/// Enqueue a message, dropped silently if the reader already closed its side.
	fn push(&self, item: T) {
		let _ = self.0.push(item);
	}
}

impl<T> Drop for QueueWriter<T> {
	fn drop(&mut self) {
		self.0.close();
	}
}

// === Virtual Streams ===

/// A virtual receive stream backed by an initial message buffer and a queue of follow-up messages.
pub struct VirtualRecvStream {
	buffer: Bytes,
	rx: Queue<Bytes>,
	closed: bool,
	park: kio::Park,
}

impl VirtualRecvStream {
	fn new(initial: Bytes, rx: Queue<Bytes>) -> Self {
		Self {
			buffer: initial,
			rx,
			closed: false,
			park: kio::Park::default(),
		}
	}

	/// Fill the buffer from the queue if empty; `self.closed` marks the FIN.
	fn poll_fill(&mut self, cx: &mut Context<'_>) -> Poll<()> {
		if !self.buffer.is_empty() || self.closed {
			return Poll::Ready(());
		}

		let waiter = self.park.hold(cx);
		match self.rx.poll_pop(waiter) {
			Poll::Ready(Some(data)) => {
				self.buffer = data;
				Poll::Ready(())
			}
			Poll::Ready(None) => {
				self.closed = true;
				Poll::Ready(())
			}
			Poll::Pending => Poll::Pending,
		}
	}
}

impl web_transport_trait::poll::RecvStream for VirtualRecvStream {
	type Error = crate::Error;

	fn poll_read(&mut self, cx: &mut Context<'_>, dst: &mut [u8]) -> Poll<Result<Option<usize>, Self::Error>> {
		if dst.is_empty() {
			return Poll::Ready(Ok(Some(0)));
		}

		std::task::ready!(self.poll_fill(cx));
		if self.buffer.is_empty() {
			return Poll::Ready(Ok(None));
		}

		let n = dst.len().min(self.buffer.len());
		dst[..n].copy_from_slice(&self.buffer[..n]);
		self.buffer.advance(n);
		Poll::Ready(Ok(Some(n)))
	}

	fn poll_read_buf<B: BufMut>(
		&mut self,
		cx: &mut Context<'_>,
		buf: &mut B,
	) -> Poll<Result<Option<usize>, Self::Error>> {
		if !buf.has_remaining_mut() {
			return Poll::Ready(Ok(Some(0)));
		}

		std::task::ready!(self.poll_fill(cx));
		if self.buffer.is_empty() {
			return Poll::Ready(Ok(None));
		}

		let n = buf.remaining_mut().min(self.buffer.len());
		buf.put(self.buffer.split_to(n));
		Poll::Ready(Ok(Some(n)))
	}

	fn poll_read_chunk(&mut self, cx: &mut Context<'_>, max: usize) -> Poll<Result<Option<Bytes>, Self::Error>> {
		if max == 0 {
			return Poll::Ready(Ok(Some(Bytes::new())));
		}

		std::task::ready!(self.poll_fill(cx));
		if self.buffer.is_empty() {
			return Poll::Ready(Ok(None));
		}

		let n = max.min(self.buffer.len());
		Poll::Ready(Ok(Some(self.buffer.split_to(n))))
	}

	fn stop(&mut self, _code: u32) {
		self.rx.close();
	}

	fn poll_closed(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
		if self.closed {
			return Poll::Ready(Ok(()));
		}

		// Drain (and discard) remaining messages until the queue closes.
		let waiter = self.park.hold(cx);
		loop {
			match self.rx.poll_pop(waiter) {
				Poll::Ready(Some(_)) => {}
				Poll::Ready(None) => {
					self.closed = true;
					return Poll::Ready(Ok(()));
				}
				Poll::Pending => return Poll::Pending,
			}
		}
	}
}

impl Drop for VirtualRecvStream {
	fn drop(&mut self) {
		// The reader is gone: close the queue so routed follow-ups are discarded
		// instead of buffering forever while the map entry lingers.
		self.rx.close();
	}
}

/// A virtual send stream that forwards writes to the shared control stream writer.
///
/// For streams created by `open_bi` (outgoing requests), this also parses
/// the first outgoing message to extract the request_id and register the
/// stream for response routing.
pub struct VirtualSendStream {
	control_tx: Queue<Bytes>,
	/// Present only for outgoing requests (from open_bi).
	/// Accumulates bytes until the request_id can be parsed,
	/// then registers the stream and flushes.
	pending: Option<OutgoingRegistration>,
}

struct OutgoingRegistration {
	follow_tx: QueueWriter<Bytes>,
	shared: Arc<Shared>,
	version: Version,
	buf: BytesMut,
}

impl OutgoingRegistration {
	/// Try to parse the request_id (and optionally namespace) from the accumulated bytes.
	/// Returns Ok(None) if not enough data yet, Err if the message is malformed.
	fn try_parse(&self) -> Result<Option<RequestId>, crate::Error> {
		let mut cursor = std::io::Cursor::new(&self.buf);
		let Ok(type_id) = u64::decode(&mut cursor, self.version) else {
			return Ok(None);
		};
		let Ok(size) = u16::decode(&mut cursor, self.version) else {
			return Ok(None);
		};

		// We know the full message size now: header bytes + body.
		let header_len = cursor.position() as usize;
		let message_len = header_len + size as usize;
		if self.buf.len() < message_len {
			return Ok(None);
		}

		// We have enough bytes for the full message; decoding must succeed.
		let request_id = RequestId::decode(&mut cursor, self.version)?;

		// For PublishNamespace, also extract the namespace for reverse lookup.
		if type_id == ietf::PublishNamespace::ID {
			if self.version == Version::Draft17 {
				// v17 has required_request_id_delta after request_id
				let _ = u64::decode(&mut cursor, self.version);
			}
			if let Ok(ns) = crate::ietf::namespace::decode_namespace(&mut cursor, self.version) {
				self.shared.namespaces.insert(Direction::Outgoing, ns, request_id);
			}
		}

		Ok(Some(request_id))
	}

	fn register(self, request_id: RequestId) {
		self.shared.streams.lock().unwrap().insert(request_id, self.follow_tx);
	}
}

impl VirtualSendStream {
	fn new(control_tx: Queue<Bytes>) -> Self {
		Self {
			control_tx,
			pending: None,
		}
	}

	fn with_registration(control_tx: Queue<Bytes>, pending: OutgoingRegistration) -> Self {
		Self {
			control_tx,
			pending: Some(pending),
		}
	}
}

impl VirtualSendStream {
	/// Route a chunk: accumulate while the request_id is still unknown, then
	/// register and flush; forward directly afterwards. Never blocks, since the
	/// control queue is unbounded.
	fn push(&mut self, chunk: Bytes) -> Result<(), crate::Error> {
		if let Some(pending) = &mut self.pending {
			pending.buf.extend_from_slice(&chunk);

			if let Some(request_id) = pending.try_parse()? {
				let mut pending = self.pending.take().unwrap();
				let buf = std::mem::take(&mut pending.buf).freeze();
				pending.register(request_id);
				if !self.control_tx.push(buf) {
					return Err(crate::Error::Closed);
				}
			}
		} else if !self.control_tx.push(chunk) {
			return Err(crate::Error::Closed);
		}

		Ok(())
	}
}

impl web_transport_trait::poll::SendStream for VirtualSendStream {
	type Error = crate::Error;

	fn poll_write(&mut self, _cx: &mut Context<'_>, buf: &[u8]) -> Poll<Result<usize, Self::Error>> {
		let len = buf.len();
		Poll::Ready(self.push(Bytes::copy_from_slice(buf)).map(|()| len))
	}

	fn poll_write_buf<B: Buf>(&mut self, _cx: &mut Context<'_>, buf: &mut B) -> Poll<Result<usize, Self::Error>> {
		// Taking the whole buffer is safe because this never returns Pending; a
		// `Bytes` source hands its chunk over without a copy.
		let len = buf.remaining();
		let chunk = buf.copy_to_bytes(len);
		Poll::Ready(self.push(chunk).map(|()| len))
	}

	fn set_priority(&mut self, _order: u8) {}

	fn finish(&mut self) -> Result<(), Self::Error> {
		// Flush any remaining buffered data (e.g. if registration never completed).
		if let Some(pending) = self.pending.take()
			&& !pending.buf.is_empty()
		{
			let _ = self.control_tx.push(pending.buf.freeze());
		}
		Ok(())
	}

	fn reset(&mut self, _code: u32) {}

	fn poll_closed(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
		Poll::Ready(Ok(()))
	}
}

// === Adapter Send/Recv Enums ===

pub enum AdapterSend<S: crate::transport::poll::Session> {
	Real(S::SendStream),
	Virtual(VirtualSendStream),
}

impl<S: crate::transport::poll::Session> web_transport_trait::poll::SendStream for AdapterSend<S> {
	type Error = crate::Error;

	fn poll_write(&mut self, cx: &mut Context<'_>, buf: &[u8]) -> Poll<Result<usize, Self::Error>> {
		match self {
			Self::Real(s) => s.poll_write(cx, buf).map(|r| r.map_err(|_| crate::Error::Closed)),
			Self::Virtual(s) => s.poll_write(cx, buf),
		}
	}

	fn poll_write_buf<B: Buf>(&mut self, cx: &mut Context<'_>, buf: &mut B) -> Poll<Result<usize, Self::Error>> {
		match self {
			Self::Real(s) => s.poll_write_buf(cx, buf).map(|r| r.map_err(|_| crate::Error::Closed)),
			Self::Virtual(s) => s.poll_write_buf(cx, buf),
		}
	}

	fn set_priority(&mut self, order: u8) {
		match self {
			Self::Real(s) => s.set_priority(order),
			Self::Virtual(s) => s.set_priority(order),
		}
	}

	fn finish(&mut self) -> Result<(), Self::Error> {
		match self {
			Self::Real(s) => s.finish().map_err(|_| crate::Error::Closed),
			Self::Virtual(s) => s.finish(),
		}
	}

	fn reset(&mut self, code: u32) {
		match self {
			Self::Real(s) => s.reset(code),
			Self::Virtual(s) => s.reset(code),
		}
	}

	fn poll_closed(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
		match self {
			Self::Real(s) => s.poll_closed(cx).map(|r| r.map_err(|_| crate::Error::Closed)),
			Self::Virtual(s) => s.poll_closed(cx),
		}
	}
}

pub enum AdapterRecv<S: crate::transport::poll::Session> {
	Real(S::RecvStream),
	Virtual(VirtualRecvStream),
}

impl<S: crate::transport::poll::Session> web_transport_trait::poll::RecvStream for AdapterRecv<S> {
	type Error = crate::Error;

	fn poll_read(&mut self, cx: &mut Context<'_>, dst: &mut [u8]) -> Poll<Result<Option<usize>, Self::Error>> {
		match self {
			Self::Real(s) => s.poll_read(cx, dst).map(|r| r.map_err(|_| crate::Error::Closed)),
			Self::Virtual(s) => s.poll_read(cx, dst),
		}
	}

	fn poll_read_buf<B: BufMut>(
		&mut self,
		cx: &mut Context<'_>,
		buf: &mut B,
	) -> Poll<Result<Option<usize>, Self::Error>> {
		match self {
			Self::Real(s) => s.poll_read_buf(cx, buf).map(|r| r.map_err(|_| crate::Error::Closed)),
			Self::Virtual(s) => s.poll_read_buf(cx, buf),
		}
	}

	fn poll_read_chunk(&mut self, cx: &mut Context<'_>, max: usize) -> Poll<Result<Option<Bytes>, Self::Error>> {
		match self {
			Self::Real(s) => s.poll_read_chunk(cx, max).map(|r| r.map_err(|_| crate::Error::Closed)),
			Self::Virtual(s) => s.poll_read_chunk(cx, max),
		}
	}

	fn stop(&mut self, code: u32) {
		match self {
			Self::Real(s) => s.stop(code),
			Self::Virtual(s) => s.stop(code),
		}
	}

	fn poll_closed(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
		match self {
			Self::Real(s) => s.poll_closed(cx).map(|r| r.map_err(|_| crate::Error::Closed)),
			Self::Virtual(s) => s.poll_closed(cx),
		}
	}
}

// === Control Stream Adapter ===

/// Which side of the session advertised a namespace.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Direction {
	/// We advertised it to the peer, on a stream from `open_bi`.
	Outgoing,
	/// The peer advertised it to us, on a stream handed to `accept_bi`.
	Incoming,
}

/// Namespace → request_id reverse lookup for the v14/v15 messages that name a
/// namespace instead of a request id.
///
/// Direction is part of the key because a cluster mesh has both endpoints
/// advertising the same namespace to each other. One map would let the second
/// advertisement overwrite the first, and a terminal message would then tear
/// down the stream travelling the other way while its real target stayed open.
#[derive(Default)]
struct Namespaces {
	outgoing: Mutex<HashMap<PathOwned, RequestId>>,
	incoming: Mutex<HashMap<PathOwned, RequestId>>,
}

impl Namespaces {
	fn map(&self, direction: Direction) -> &Mutex<HashMap<PathOwned, RequestId>> {
		match direction {
			Direction::Outgoing => &self.outgoing,
			Direction::Incoming => &self.incoming,
		}
	}

	fn insert(&self, direction: Direction, namespace: PathOwned, request_id: RequestId) {
		self.map(direction).lock().unwrap().insert(namespace, request_id);
	}

	fn get(&self, direction: Direction, namespace: &PathOwned) -> Option<RequestId> {
		self.map(direction).lock().unwrap().get(namespace).copied()
	}
}

#[derive(Default)]
struct Shared {
	/// New virtual streams for accept_bi; any caller can pop directly.
	incoming: Queue<(VirtualSendStream, VirtualRecvStream)>,

	/// Messages every VirtualSendStream writes; the run_write task drains this
	/// onto the real control stream.
	control: Queue<Bytes>,

	/// Active virtual streams keyed by request_id. Removing an entry drops its
	/// writer, which FINs the matching VirtualRecvStream.
	streams: Mutex<HashMap<RequestId, QueueWriter<Bytes>>>,

	/// Namespace → request_id reverse lookup (for v14/v15 namespace-keyed messages).
	namespaces: Namespaces,
}

impl Shared {
	/// Mint the local half of a request we're about to send. Registration waits
	/// until the first write reveals the request_id.
	fn open_outgoing(self: &Arc<Self>, version: Version) -> (VirtualSendStream, VirtualRecvStream) {
		let follow = Queue::new();
		let recv = VirtualRecvStream::new(Bytes::new(), follow.clone());
		let send = VirtualSendStream::with_registration(
			self.control.clone(),
			OutgoingRegistration {
				follow_tx: follow.writer(),
				shared: Arc::clone(self),
				version,
				buf: BytesMut::new(),
			},
		);
		(send, recv)
	}

	/// Register the peer's new request and queue its stream for accept_bi.
	fn open_incoming(&self, request_id: RequestId, raw: Bytes) -> Result<(), Error> {
		let follow = Queue::new();
		let recv = VirtualRecvStream::new(raw, follow.clone());
		let send = VirtualSendStream::new(self.control.clone());
		self.streams.lock().unwrap().insert(request_id, follow.writer());
		if !self.incoming.push((send, recv)) {
			return Err(Error::Closed);
		}
		Ok(())
	}

	/// Deliver a message to an open virtual stream, dropping it if the stream is gone.
	fn push(&self, request_id: RequestId, raw: Bytes) {
		if let Some(tx) = self.streams.lock().unwrap().get(&request_id) {
			tx.push(raw);
		}
	}

	/// Deliver a final message and FIN the stream: the removed writer drops after it.
	fn close(&self, request_id: RequestId, raw: Bytes) {
		if let Some(tx) = self.streams.lock().unwrap().remove(&request_id) {
			tx.push(raw);
		}
	}
}

#[derive(Clone)]
pub struct ControlStreamAdapter<S: crate::transport::poll::Session> {
	inner: S,
	shared: Arc<Shared>,
	control: Control,
	version: Version,
	// Bridges Context-based polls onto the kio queues; empty on clone.
	park: kio::Park,
}

impl<S: crate::transport::poll::Session> ControlStreamAdapter<S> {
	pub fn new(inner: S, control: Control, version: Version) -> Self {
		Self {
			inner,
			shared: Arc::new(Shared::default()),
			control,
			version,
			park: kio::Park::default(),
		}
	}

	/// Open a real (non-virtual) bidi stream, bypassing control stream multiplexing.
	/// Used for v16 SubscribeNamespace which moved to its own bidi stream.
	pub async fn open_native_bi(&mut self) -> Result<(AdapterSend<S>, AdapterRecv<S>), crate::Error> {
		let (send, recv) = self.inner.open_bi().await.map_err(|_| crate::Error::Closed)?;
		Ok((AdapterSend::Real(send), AdapterRecv::Real(recv)))
	}

	/// Run the control stream read + write tasks.
	/// This reads from the control stream and routes messages to virtual streams,
	/// and also drains the write queue to the control stream writer. A decoded
	/// GOAWAY is surfaced through `goaway` so [`crate::Session::recv_goaway`] resolves
	/// instead of the session tearing down.
	pub async fn run(
		&self,
		reader: Reader<S::RecvStream, Version>,
		writer: Writer<S::SendStream, Version>,
		goaway: crate::goaway::Protocol,
	) -> Result<(), Error> {
		let res = {
			let mut read = std::pin::pin!(self.run_read(reader, goaway));
			let mut write = std::pin::pin!(self.run_write(writer));
			kio::wait(|waiter| {
				if let Poll::Ready(res) = waiter.poll_future(read.as_mut()) {
					return Poll::Ready(res);
				}
				if let Poll::Ready(res) = waiter.poll_future(write.as_mut()) {
					return Poll::Ready(res);
				}
				Poll::Pending
			})
			.await
		};

		// The control stream is dead, so the mux is too: fail future virtual
		// writes fast and end accept_bi instead of buffering into a queue nobody
		// will ever drain.
		self.shared.control.close();
		self.shared.incoming.close();

		res
	}

	/// Queue a GOAWAY message onto the shared control stream (draft-14-16).
	///
	/// Framed as `[type_id varint][size u16][body]` like every other control
	/// message. Best-effort: an encode failure or a closed control stream is
	/// logged and dropped, since the session is tearing down anyway.
	pub(super) fn send_goaway(&self, uri: &str, timeout_ms: u64, version: Version) {
		use crate::coding::Encode;

		let msg = crate::ietf::GoAway {
			new_session_uri: std::borrow::Cow::Borrowed(uri),
			timeout: timeout_ms,
		};

		let mut body = BytesMut::new();
		if let Err(err) = msg.encode_msg(&mut body, version) {
			tracing::warn!(%err, "failed to encode goaway");
			return;
		}

		// The size prefix is a u16 on a stream shared with every other request, so a
		// wrapping cast here would desynchronize the framing for all of them.
		let Ok(size) = u16::try_from(body.len()) else {
			tracing::warn!(len = body.len(), "goaway too large for the control stream");
			return;
		};

		let mut raw = BytesMut::new();
		if crate::ietf::GoAway::ID.encode(&mut raw, version).is_err() || size.encode(&mut raw, version).is_err() {
			return;
		}
		raw.extend_from_slice(&body);

		if !self.shared.control.push(raw.freeze()) {
			tracing::debug!("control stream closed; goaway not sent");
		}
	}

	/// Writer task: drains the queue and writes to the control stream.
	async fn run_write(&self, mut writer: Writer<S::SendStream, Version>) -> Result<(), Error> {
		while let Some(msg) = self.shared.control.pop().await {
			let mut buf = std::io::Cursor::new(msg);
			writer.write_all(&mut buf).await?;
		}
		Ok(())
	}

	/// Dispatcher loop that reads control stream messages and routes them.
	async fn run_read(
		&self,
		mut reader: Reader<S::RecvStream, Version>,
		goaway: crate::goaway::Protocol,
	) -> Result<(), Error> {
		loop {
			let type_id: u64 = match reader.decode_maybe().await? {
				Some(id) => id,
				None => return Ok(()),
			};

			let size: u16 = reader.decode::<u16>().await?;

			let body = reader.read_exact(size as usize).await?;

			// Reconstruct raw message bytes: [type_id][size][body]
			let raw = encode_raw(type_id, size, &body, self.version);

			// Classify and route
			match classify(type_id, &body, self.version, &self.shared.namespaces)? {
				Route::NewRequest(request_id) => self.shared.open_incoming(request_id, raw)?,
				Route::Response(request_id) | Route::FollowUp(request_id) => self.shared.push(request_id, raw),
				Route::CloseStream(request_id) => self.shared.close(request_id, raw),
				Route::MaxRequestId(max) => self.control.max_request_id(max),
				Route::GoAway => {
					let mut data = body;
					let msg = crate::ietf::GoAway::decode_msg(&mut data, self.version)?;
					tracing::info!(message = ?msg, "received GOAWAY");

					// Draft-14-16 have no timeout field.
					// A second GOAWAY is a protocol violation the draft requires we
					// close over, so let it propagate and end the session.
					goaway.record(crate::goaway::Goaway {
						uri: msg.new_session_uri.into_owned(),
						timeout: None,
					})?;
					// Keep processing control messages: existing subscriptions
					// flow until the session actually closes.
				}
			}
		}
	}
}

/// Classify a control message and extract its request_id for routing.
///
/// Takes the namespace map rather than reading it off the adapter so the routing
/// rules can be tested without a session behind them.
fn classify(type_id: u64, body: &Bytes, version: Version, namespaces: &Namespaces) -> Result<Route, Error> {
	match type_id {
		// New requests: these create new virtual streams
		ietf::Subscribe::ID => {
			let id = decode_request_id(body, version)?;
			Ok(Route::NewRequest(id))
		}
		ietf::Fetch::ID => {
			let id = decode_request_id(body, version)?;
			Ok(Route::NewRequest(id))
		}
		ietf::Publish::ID => {
			let id = decode_request_id(body, version)?;
			Ok(Route::NewRequest(id))
		}
		ietf::PublishNamespace::ID => {
			let id = decode_request_id(body, version)?;
			// Decode the namespace and store the mapping for v14/v15 reverse lookup
			if let Ok(ns) = decode_publish_namespace_body(body, version) {
				namespaces.insert(Direction::Incoming, ns, id);
			}
			Ok(Route::NewRequest(id))
		}
		ietf::TrackStatus::ID => {
			let id = decode_request_id(body, version)?;
			Ok(Route::NewRequest(id))
		}
		// SubscribeNamespace on control stream (v14/v15 only)
		ietf::SubscribeNamespaceLegacy::ID => match version {
			Version::Draft14 | Version::Draft15 => {
				let id = decode_request_id(body, version)?;
				Ok(Route::NewRequest(id))
			}
			_ => Err(Error::UnexpectedMessage),
		},

		// SUBSCRIBE_TRACKS (draft-18+, #1542): the half of the SUBSCRIBE_NAMESPACE split
		// that subscribes to all tracks under a prefix. We don't implement PUBLISH
		// replication, so this is impossible to honor. Reject loudly.
		ietf::SUBSCRIBE_TRACKS_ID => {
			tracing::error!(
				version = ?version,
				"received SUBSCRIBE_TRACKS (0x51); not supported in moq-lite (no PUBLISH replication)"
			);
			Err(Error::Unsupported)
		}

		// Responses: route to the virtual stream waiting for a reply
		ietf::SubscribeOk::ID => {
			let id = decode_response_request_id(body, version)?;
			Ok(Route::Response(id))
		}
		// 0x05: SubscribeError in v14, RequestError in v15+
		ietf::SubscribeError::ID => {
			let id = decode_response_request_id(body, version)?;
			Ok(Route::CloseStream(id))
		}
		ietf::FetchOk::ID => {
			let id = decode_response_request_id(body, version)?;
			Ok(Route::Response(id))
		}
		// 0x19: FetchError in v14 only
		ietf::FetchError::ID => match version {
			Version::Draft14 => {
				let id = decode_request_id(body, version)?;
				Ok(Route::CloseStream(id))
			}
			_ => Err(Error::UnexpectedMessage),
		},
		// PublishOk (0x1E)
		ietf::PublishOk::ID => {
			let id = decode_response_request_id(body, version)?;
			Ok(Route::Response(id))
		}
		// PublishError (0x1F) - v14 only
		ietf::PublishError::ID => {
			let id = decode_request_id(body, version)?;
			Ok(Route::CloseStream(id))
		}
		// 0x07: PublishNamespaceOk in v14, RequestOk in v15+
		ietf::PublishNamespaceOk::ID => match version {
			Version::Draft14 => {
				let id = decode_request_id(body, version)?;
				Ok(Route::Response(id))
			}
			Version::Draft15 | Version::Draft16 => {
				// RequestOk - route to stream
				let id = decode_response_request_id(body, version)?;
				Ok(Route::Response(id))
			}
			_ => Err(Error::UnexpectedMessage),
		},
		// 0x08: PublishNamespaceError in v14 only
		ietf::PublishNamespaceError::ID => match version {
			Version::Draft14 => {
				let id = decode_request_id(body, version)?;
				Ok(Route::CloseStream(id))
			}
			_ => Err(Error::UnexpectedMessage),
		},
		// SubscribeNamespaceOk (v14 only)
		ietf::SubscribeNamespaceOk::ID => match version {
			Version::Draft14 => {
				let id = decode_request_id(body, version)?;
				Ok(Route::Response(id))
			}
			_ => Err(Error::UnexpectedMessage),
		},
		// SubscribeNamespaceError (v14 only)
		ietf::SubscribeNamespaceError::ID => match version {
			Version::Draft14 => {
				let id = decode_request_id(body, version)?;
				Ok(Route::CloseStream(id))
			}
			_ => Err(Error::UnexpectedMessage),
		},

		// Follow-up messages: route to existing stream
		ietf::SubscribeUpdate::ID => {
			let id = decode_request_id(body, version)?;
			Ok(Route::FollowUp(id))
		}

		// Close stream messages
		ietf::Unsubscribe::ID => {
			let id = decode_request_id(body, version)?;
			Ok(Route::CloseStream(id))
		}
		ietf::PublishDone::ID => {
			let id = decode_response_request_id(body, version)?;
			Ok(Route::CloseStream(id))
		}
		ietf::FetchCancel::ID => {
			let id = decode_request_id(body, version)?;
			Ok(Route::CloseStream(id))
		}
		ietf::PublishNamespaceDone::ID => match version {
			Version::Draft16 => {
				let id = decode_request_id(body, version)?;
				Ok(Route::CloseStream(id))
			}
			// v14/v15: namespace-keyed, so decode namespace and look up request_id.
			// DONE withdraws the advertisement its sender made, so it names an
			// inbound one.
			Version::Draft14 | Version::Draft15 => {
				let id = lookup_namespace_request_id(body, version, namespaces, Direction::Incoming)?;
				Ok(Route::CloseStream(id))
			}
			_ => Err(Error::UnexpectedMessage),
		},
		ietf::PublishNamespaceCancel::ID => match version {
			Version::Draft16 => {
				let id = decode_request_id(body, version)?;
				Ok(Route::CloseStream(id))
			}
			// v14/v15: namespace-keyed. CANCEL rejects an advertisement its sender
			// received, so it names an outbound one.
			Version::Draft14 | Version::Draft15 => {
				let id = lookup_namespace_request_id(body, version, namespaces, Direction::Outgoing)?;
				Ok(Route::CloseStream(id))
			}
			_ => Err(Error::UnexpectedMessage),
		},
		ietf::UnsubscribeNamespace::ID => match version {
			Version::Draft14 | Version::Draft15 => {
				let id = decode_request_id(body, version)?;
				Ok(Route::CloseStream(id))
			}
			_ => Err(Error::UnexpectedMessage),
		},

		// Utility
		ietf::MaxRequestId::ID => {
			let id = decode_request_id(body, version)?;
			Ok(Route::MaxRequestId(id))
		}
		ietf::RequestsBlocked::ID => Err(Error::UnexpectedMessage),

		// Terminal
		ietf::GoAway::ID => Ok(Route::GoAway),

		_ => Err(Error::UnexpectedMessage),
	}
}

/// Decode the namespace from a v14/v15 namespace-keyed message body and look up the
/// request_id of the advertisement it terminates, travelling in the given direction.
fn lookup_namespace_request_id(
	body: &Bytes,
	version: Version,
	namespaces: &Namespaces,
	direction: Direction,
) -> Result<RequestId, Error> {
	let mut cursor = std::io::Cursor::new(body);
	let ns = crate::ietf::namespace::decode_namespace(&mut cursor, version)?;
	namespaces.get(direction, &ns).ok_or(Error::NotFound)
}

impl<S: crate::transport::poll::Session> web_transport_trait::poll::Session for ControlStreamAdapter<S> {
	type SendStream = AdapterSend<S>;
	type RecvStream = AdapterRecv<S>;
	type Error = crate::Error;

	fn poll_accept_bi(
		&mut self,
		cx: &mut Context<'_>,
	) -> Poll<Result<(Self::SendStream, Self::RecvStream), Self::Error>> {
		// v16: SubscribeNamespace uses real bidi streams, so poll both sources.
		// Native first: real bidi streams are rare and transport-paced, so they
		// can't starve the queue, while a control-message flood could starve a
		// native SubscribeNamespace under queue-first order.
		if self.version == Version::Draft16 {
			match self.inner.poll_accept_bi(cx) {
				Poll::Ready(Ok((send, recv))) => {
					return Poll::Ready(Ok((AdapterSend::Real(send), AdapterRecv::Real(recv))));
				}
				Poll::Ready(Err(_)) => return Poll::Ready(Err(crate::Error::Closed)),
				Poll::Pending => {}
			}
		}

		let waiter = self.park.hold(cx);
		match std::task::ready!(self.shared.incoming.poll_pop(waiter)) {
			Some((send, recv)) => Poll::Ready(Ok((AdapterSend::Virtual(send), AdapterRecv::Virtual(recv)))),
			None => Poll::Ready(Err(crate::Error::Closed)),
		}
	}

	fn poll_open_bi(
		&mut self,
		_cx: &mut Context<'_>,
	) -> Poll<Result<(Self::SendStream, Self::RecvStream), Self::Error>> {
		let (send, recv) = self.shared.open_outgoing(self.version);
		Poll::Ready(Ok((AdapterSend::Virtual(send), AdapterRecv::Virtual(recv))))
	}

	fn poll_open_uni(&mut self, cx: &mut Context<'_>) -> Poll<Result<Self::SendStream, Self::Error>> {
		self.inner
			.poll_open_uni(cx)
			.map(|r| r.map(AdapterSend::Real).map_err(|_| crate::Error::Closed))
	}

	fn poll_accept_uni(&mut self, cx: &mut Context<'_>) -> Poll<Result<Self::RecvStream, Self::Error>> {
		self.inner
			.poll_accept_uni(cx)
			.map(|r| r.map(AdapterRecv::Real).map_err(|_| crate::Error::Closed))
	}

	fn poll_send_datagram(&mut self, cx: &mut Context<'_>, payload: &[u8]) -> Poll<Result<(), Self::Error>> {
		self.inner
			.poll_send_datagram(cx, payload)
			.map(|r| r.map_err(|_| crate::Error::Closed))
	}

	fn poll_recv_datagram(&mut self, cx: &mut Context<'_>) -> Poll<Result<Bytes, Self::Error>> {
		self.inner
			.poll_recv_datagram(cx)
			.map(|r| r.map_err(|_| crate::Error::Closed))
	}

	fn max_datagram_size(&self) -> usize {
		self.inner.max_datagram_size()
	}

	fn protocol(&self) -> Option<&str> {
		self.inner.protocol()
	}

	fn close(&mut self, code: u32, reason: &str) {
		self.inner.close(code, reason)
	}

	fn poll_closed(&mut self, cx: &mut Context<'_>) -> Poll<Self::Error> {
		self.inner.poll_closed(cx).map(|_| crate::Error::Closed)
	}

	fn stats(&self) -> impl web_transport_trait::Stats {
		self.inner.stats()
	}
}

// === Message Classification ===

#[derive(Debug)]
enum Route {
	NewRequest(RequestId),
	Response(RequestId),
	FollowUp(RequestId),
	CloseStream(RequestId),
	MaxRequestId(RequestId),
	GoAway,
}

/// Encode raw message bytes as [type_id varint][size u16][body].
fn encode_raw(type_id: u64, size: u16, body: &Bytes, version: Version) -> Bytes {
	let mut buf = BytesMut::new();
	type_id.encode(&mut buf, version).expect("encode type_id");
	size.encode(&mut buf, version).expect("encode size");
	buf.extend_from_slice(body);
	buf.freeze()
}

/// Decode just the request_id from the beginning of a message body.
fn decode_request_id(body: &Bytes, version: Version) -> Result<RequestId, Error> {
	let mut cursor = std::io::Cursor::new(body);
	let request_id = RequestId::decode(&mut cursor, version)?;
	Ok(request_id)
}

/// Decode request_id for response messages that have Option<RequestId> in v14-16.
fn decode_response_request_id(body: &Bytes, version: Version) -> Result<RequestId, Error> {
	// In v14-16, response messages always have request_id present
	decode_request_id(body, version)
}

/// Decode the namespace from a PublishNamespace message body (after the request_id).
fn decode_publish_namespace_body(body: &Bytes, version: Version) -> Result<PathOwned, Error> {
	let mut cursor = std::io::Cursor::new(body);
	// Skip request_id
	let _request_id = RequestId::decode(&mut cursor, version)?;
	// v17 has required_request_id_delta
	if version == Version::Draft17 {
		let _ = u64::decode(&mut cursor, version)?;
	}
	let ns = crate::ietf::namespace::decode_namespace(&mut cursor, version)?;
	Ok(ns.into_owned())
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::transport::poll::{RecvStream as _, SendStream as _};
	use bytes::BytesMut;
	use futures::FutureExt as _;

	fn make_body_with_request_id(id: u64, version: Version) -> Bytes {
		let mut buf = BytesMut::new();
		RequestId(id).encode(&mut buf, version).unwrap();
		buf.freeze()
	}

	/// Classify against an empty namespace map, for the messages that don't use it.
	fn classify_msg(version: Version, type_id: u64, body: &Bytes) -> Result<Route, Error> {
		classify(type_id, body, version, &Namespaces::default())
	}

	#[test]
	fn test_classify_subscribe_new_request() {
		let body = make_body_with_request_id(42, Version::Draft15);
		let route = classify_msg(Version::Draft15, ietf::Subscribe::ID, &body).unwrap();
		assert!(matches!(route, Route::NewRequest(RequestId(42))));
	}

	#[test]
	fn test_classify_fetch_new_request() {
		let body = make_body_with_request_id(10, Version::Draft14);
		let route = classify_msg(Version::Draft14, ietf::Fetch::ID, &body).unwrap();
		assert!(matches!(route, Route::NewRequest(RequestId(10))));
	}

	#[test]
	fn test_classify_publish_new_request() {
		let body = make_body_with_request_id(5, Version::Draft16);
		let route = classify_msg(Version::Draft16, ietf::Publish::ID, &body).unwrap();
		assert!(matches!(route, Route::NewRequest(RequestId(5))));
	}

	#[test]
	fn test_classify_subscribe_ok_response() {
		let body = make_body_with_request_id(42, Version::Draft15);
		let route = classify_msg(Version::Draft15, ietf::SubscribeOk::ID, &body).unwrap();
		assert!(matches!(route, Route::Response(RequestId(42))));
	}

	#[test]
	fn test_classify_request_error_v15_closes_stream() {
		let body = make_body_with_request_id(7, Version::Draft15);
		let route = classify_msg(Version::Draft15, ietf::SubscribeError::ID, &body).unwrap();
		assert!(matches!(route, Route::CloseStream(RequestId(7))));
	}

	#[test]
	fn test_classify_request_ok_v15_response() {
		let body = make_body_with_request_id(3, Version::Draft15);
		let route = classify_msg(Version::Draft15, ietf::PublishNamespaceOk::ID, &body).unwrap();
		assert!(matches!(route, Route::Response(RequestId(3))));
	}

	#[test]
	fn test_classify_unsubscribe_closes_stream() {
		let body = make_body_with_request_id(99, Version::Draft14);
		let route = classify_msg(Version::Draft14, ietf::Unsubscribe::ID, &body).unwrap();
		assert!(matches!(route, Route::CloseStream(RequestId(99))));
	}

	#[test]
	fn test_classify_subscribe_update_followup() {
		let body = make_body_with_request_id(10, Version::Draft15);
		let route = classify_msg(Version::Draft15, ietf::SubscribeUpdate::ID, &body).unwrap();
		assert!(matches!(route, Route::FollowUp(RequestId(10))));
	}

	#[test]
	fn test_classify_goaway() {
		let body = Bytes::new();
		let route = classify_msg(Version::Draft14, ietf::GoAway::ID, &body).unwrap();
		assert!(matches!(route, Route::GoAway));
	}

	#[test]
	fn test_classify_max_request_id() {
		let body = make_body_with_request_id(100, Version::Draft14);
		let route = classify_msg(Version::Draft14, ietf::MaxRequestId::ID, &body).unwrap();
		assert!(matches!(route, Route::MaxRequestId(RequestId(100))));
	}

	#[test]
	fn test_classify_subscribe_namespace_v14_new_request() {
		let body = make_body_with_request_id(20, Version::Draft14);
		let route = classify_msg(Version::Draft14, ietf::SubscribeNamespaceLegacy::ID, &body).unwrap();
		assert!(matches!(route, Route::NewRequest(RequestId(20))));
	}

	#[test]
	fn test_classify_subscribe_namespace_v16_errors() {
		let body = make_body_with_request_id(20, Version::Draft16);
		let result = classify_msg(Version::Draft16, ietf::SubscribeNamespaceLegacy::ID, &body);
		assert!(result.is_err());
	}

	#[test]
	fn test_classify_unknown_message() {
		let body = Bytes::new();
		let result = classify_msg(Version::Draft14, 0xFF, &body);
		assert!(result.is_err());
	}

	#[test]
	fn test_encode_raw_roundtrip() {
		let version = Version::Draft15;
		let body = Bytes::from_static(b"hello");
		let raw = encode_raw(0x03, 5, &body, version);

		// Decode the raw bytes
		let mut cursor = std::io::Cursor::new(&raw[..]);
		let type_id = u64::decode(&mut cursor, version).unwrap();
		let size = u16::decode(&mut cursor, version).unwrap();
		assert_eq!(type_id, 0x03);
		assert_eq!(size, 5);
	}

	#[tokio::test]
	async fn test_virtual_recv_stream_reads_initial_then_followup() {
		let initial = Bytes::from_static(b"initial");
		let follow = Queue::new();
		let tx = follow.writer();
		let mut stream = VirtualRecvStream::new(initial, follow);

		// Read initial data
		let mut buf = [0u8; 32];
		let n = stream.read(&mut buf).await.unwrap().unwrap();
		assert_eq!(&buf[..n], b"initial");

		// Send follow-up
		tx.push(Bytes::from_static(b"followup"));
		let n = stream.read(&mut buf).await.unwrap().unwrap();
		assert_eq!(&buf[..n], b"followup");

		// Drop the writer → FIN
		drop(tx);
		let result = stream.read(&mut buf).await.unwrap();
		assert_eq!(result, None);
	}

	#[tokio::test]
	async fn test_virtual_recv_stream_partial_reads() {
		let initial = Bytes::from_static(b"hello world");
		let mut stream = VirtualRecvStream::new(initial, Queue::new());

		// Read small chunks
		let mut buf = [0u8; 5];
		let n = stream.read(&mut buf).await.unwrap().unwrap();
		assert_eq!(&buf[..n], b"hello");

		let n = stream.read(&mut buf).await.unwrap().unwrap();
		assert_eq!(&buf[..n], b" worl");

		let mut buf = [0u8; 1];
		let n = stream.read(&mut buf).await.unwrap().unwrap();
		assert_eq!(&buf[..n], b"d");
	}

	#[tokio::test]
	async fn test_virtual_send_stream_errors_once_control_closes() {
		// The adapter closes the control queue when its run() exits; writes must
		// fail fast instead of buffering into a queue nobody drains.
		let control = Queue::new();
		let mut stream = VirtualSendStream::new(control.clone());
		control.close();
		assert!(matches!(stream.write(b"hello").await, Err(Error::Closed)));
	}

	#[test]
	fn test_recv_stream_drop_discards_followups() {
		// Dropping the reader closes the queue, so late routed messages are
		// discarded instead of accumulating while the map entry lingers.
		let follow = Queue::new();
		let stream = VirtualRecvStream::new(Bytes::new(), follow.clone());
		drop(stream);
		assert!(!follow.push(Bytes::from_static(b"late")));
	}

	#[tokio::test]
	async fn test_virtual_send_stream_writes_to_channel() {
		let control = Queue::new();
		let mut stream = VirtualSendStream::new(control.clone());

		let n = stream.write(b"hello").await.unwrap();
		assert_eq!(n, 5);

		let data = control.pop().await.unwrap();
		assert_eq!(data, &b"hello"[..]);
	}

	/// Encode a message body (no type_id/size header).
	fn encode_body<M: Message>(msg: &M, version: Version) -> Bytes {
		let mut buf = BytesMut::new();
		msg.encode_msg(&mut buf, version).unwrap();
		buf.freeze()
	}

	/// Encode a full control message: [type_id][size][body].
	fn encode_msg<M: Message>(msg: &M, version: Version) -> Bytes {
		let body = encode_body(msg, version);
		encode_raw(M::ID, body.len() as u16, &body, version)
	}

	fn publish_namespace(request_id: RequestId, namespace: &str) -> ietf::PublishNamespace<'_> {
		ietf::PublishNamespace {
			request_id,
			track_namespace: crate::Path::new(namespace),
			cluster: None,
		}
	}

	/// Advertise `namespace` to the peer on request 4, as `open_bi` + a write would.
	async fn advertise(shared: &Arc<Shared>, namespace: &str, version: Version) -> VirtualRecvStream {
		let (mut send, recv) = shared.open_outgoing(version);
		send.write_chunk(encode_msg(&publish_namespace(RequestId(4), namespace), version))
			.await
			.unwrap();
		recv
	}

	/// Receive the peer's advertisement of `namespace` on request 7, as the read loop would.
	async fn receive(shared: &Arc<Shared>, namespace: &str, version: Version) -> VirtualRecvStream {
		let msg = publish_namespace(RequestId(7), namespace);
		let route = classify(
			ietf::PublishNamespace::ID,
			&encode_body(&msg, version),
			version,
			&shared.namespaces,
		)
		.unwrap();
		assert!(matches!(route, Route::NewRequest(RequestId(7))));
		shared.open_incoming(RequestId(7), encode_msg(&msg, version)).unwrap();

		let (_, mut recv) = shared.incoming.pop().await.unwrap();

		// Drain the advertisement the stream opened with, so both halves are quiescent
		// and a later read only sees what the terminal message routes.
		assert_eq!(
			recv.read_chunk(usize::MAX).await.unwrap(),
			Some(encode_msg(&msg, version))
		);
		recv
	}

	/// Two relays meshed together, each advertising `namespace` to the other: ours on
	/// request 4, theirs on request 7. Returns their receive halves.
	///
	/// `last` is the advertisement registered second, which is the one a single map
	/// would leave as the sole entry. Each test passes the direction it must *not*
	/// match, so a direction-blind lookup lands on the wrong stream.
	async fn mesh(
		namespace: &str,
		version: Version,
		last: Direction,
	) -> (Arc<Shared>, VirtualRecvStream, VirtualRecvStream) {
		let shared = Arc::new(Shared::default());

		let (ours, theirs) = match last {
			Direction::Outgoing => {
				let theirs = receive(&shared, namespace, version).await;
				(advertise(&shared, namespace, version).await, theirs)
			}
			Direction::Incoming => {
				let ours = advertise(&shared, namespace, version).await;
				(ours, receive(&shared, namespace, version).await)
			}
		};

		(shared, ours, theirs)
	}

	/// Read until the stream FINs, returning whether it did so within `limit` reads.
	async fn drained(stream: &mut VirtualRecvStream, limit: usize) -> bool {
		for _ in 0..limit {
			if stream.read_chunk(usize::MAX).await.unwrap().is_none() {
				return true;
			}
		}
		false
	}

	#[test]
	fn test_namespace_reverse_lookup_v14() {
		let namespaces = Namespaces::default();
		namespaces.insert(Direction::Incoming, crate::Path::new("test/ns"), RequestId(42));

		let body = encode_body(
			&ietf::PublishNamespaceDone {
				track_namespace: crate::Path::new("test/ns"),
				request_id: RequestId(0),
			},
			Version::Draft14,
		);

		let route = classify(ietf::PublishNamespaceDone::ID, &body, Version::Draft14, &namespaces).unwrap();
		assert!(matches!(route, Route::CloseStream(RequestId(42))));
	}

	#[tokio::test]
	async fn test_publish_namespace_done_closes_only_inbound() {
		// The peer withdraws the advertisement it made, so its own stream closes and
		// ours keeps running.
		let version = Version::Draft14;
		let (shared, mut ours, mut theirs) = mesh("cluster/ns", version, Direction::Outgoing).await;

		let done = ietf::PublishNamespaceDone {
			track_namespace: crate::Path::new("cluster/ns"),
			request_id: RequestId(0),
		};
		let route = classify(
			ietf::PublishNamespaceDone::ID,
			&encode_body(&done, version),
			version,
			&shared.namespaces,
		)
		.unwrap();
		assert!(matches!(route, Route::CloseStream(RequestId(7))));

		shared.close(RequestId(7), encode_msg(&done, version));
		assert!(drained(&mut theirs, 4).await);
		assert!(shared.streams.lock().unwrap().contains_key(&RequestId(4)));
		assert!(ours.read_chunk(usize::MAX).now_or_never().is_none());
	}

	#[tokio::test]
	async fn test_publish_namespace_cancel_closes_only_outbound() {
		// The peer rejects the advertisement we made, so ours closes and the one it
		// sent us keeps running.
		let version = Version::Draft14;
		let (shared, mut ours, mut theirs) = mesh("cluster/ns", version, Direction::Incoming).await;

		let cancel = ietf::PublishNamespaceCancel {
			track_namespace: crate::Path::new("cluster/ns"),
			request_id: RequestId(0),
			error_code: 0,
			reason_phrase: "".into(),
		};
		let route = classify(
			ietf::PublishNamespaceCancel::ID,
			&encode_body(&cancel, version),
			version,
			&shared.namespaces,
		)
		.unwrap();
		assert!(matches!(route, Route::CloseStream(RequestId(4))));

		shared.close(RequestId(4), encode_msg(&cancel, version));
		assert!(drained(&mut ours, 4).await);
		assert!(shared.streams.lock().unwrap().contains_key(&RequestId(7)));
		assert!(theirs.read_chunk(usize::MAX).now_or_never().is_none());
	}
}
