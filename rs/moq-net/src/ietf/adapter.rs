use crate::driver::Transport;
use crate::transport::poll::Session as _;
use std::{
	collections::{HashMap, HashSet, VecDeque},
	sync::{Arc, Mutex, Weak},
	task::{Context, Poll},
};

use bytes::{Buf, Bytes, BytesMut};

use crate::{
	Error, PathOwned,
	coding::{Decode, Decoder, Encoder, Reader, Writer},
	ietf::{self, RequestId},
};

use super::{Control, Message, Version, control::RequestPermit};

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
	#[cfg(test)]
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

	/// Stop future pushes; buffered messages still drain, then `poll_pop` returns `None`.
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
	/// Enqueue a message, returning false if the reader already closed its side.
	fn push(&self, item: T) -> bool {
		self.0.push(item)
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
	shared: Weak<Shared>,
	/// The request this stream serves, known up front when the peer opened it and
	/// filled in by the first write when we did.
	request_id: Arc<Mutex<Option<RequestId>>>,
	/// Holds the peer's request open against our MAX_REQUEST_ID window until the
	/// handler drops this stream. Only on a stream the peer opened.
	_permit: Option<RequestPermit>,
}

impl VirtualRecvStream {
	fn new(initial: Bytes, rx: Queue<Bytes>, shared: Arc<Shared>, request_id: Arc<Mutex<Option<RequestId>>>) -> Self {
		Self {
			buffer: initial,
			rx,
			closed: false,
			park: kio::Park::default(),
			shared: Arc::downgrade(&shared),
			request_id,
			_permit: None,
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

impl crate::transport::poll::Chunks for VirtualRecvStream {
	type Error = crate::Error;

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
}

impl Drop for VirtualRecvStream {
	fn drop(&mut self) {
		// The reader is gone: close the queue so routed follow-ups are discarded
		// instead of buffering forever while the map entry lingers.
		self.rx.close();

		// Release the routing entries too, so a request torn down locally gives its
		// namespace back and the same one can be advertised again.
		if let Some(request_id) = *self.request_id.lock().unwrap()
			&& let Some(shared) = self.shared.upgrade()
		{
			shared.forget(request_id);
		}
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
	/// Draft-16 only, once registered: what follows the request, held until whole so
	/// each REQUEST_UPDATE is routed back to its request before it leaves.
	updates: Option<UpdateRouting>,
}

struct OutgoingRegistration {
	follow_tx: QueueWriter<Bytes>,
	shared: Arc<Shared>,
	version: Version,
	buf: BytesMut,
	/// Shared with the matching [`VirtualRecvStream`] so it can release this request's
	/// routing entries once the reader is dropped.
	request_id: Arc<Mutex<Option<RequestId>>>,
}

impl OutgoingRegistration {
	/// Try to parse the request_id (and optionally namespace) from the accumulated bytes,
	/// with the length of the message that carried it.
	/// Returns Ok(None) if not enough data yet, Err if the message is malformed.
	fn try_parse(&self) -> Result<Option<(RequestId, usize)>, crate::Error> {
		let Some((type_id, len, mut body)) = whole_message(&self.buf, self.version) else {
			return Ok(None);
		};

		// We have enough bytes for the full message; decoding must succeed.
		let request_id = RequestId::decode(&mut body, self.version)?;

		// For PublishNamespace, also extract the namespace for reverse lookup.
		if type_id == ietf::PublishNamespace::ID {
			if self.version == Version::Draft17 {
				// v17 has required_request_id_delta after request_id
				let _ = body.varint();
			}
			if let Ok(ns) = crate::ietf::namespace::decode_namespace(&mut body) {
				self.shared.namespaces.register(Direction::Outgoing, ns, request_id);
			}
		}

		Ok(Some((request_id, len)))
	}

	fn register(self, request_id: RequestId) {
		// Serialize publication with receive-half cleanup, including a drop before
		// the first write supplied the request id.
		let mut registered = self.request_id.lock().unwrap();
		*registered = Some(request_id);
		let closed = self.follow_tx.0.0.lock().closed;
		if closed {
			self.shared.forget(request_id);
		} else {
			self.shared.streams.lock().unwrap().insert(request_id, self.follow_tx);
		}
	}
}

impl VirtualSendStream {
	fn new(control_tx: Queue<Bytes>) -> Self {
		Self {
			control_tx,
			pending: None,
			updates: None,
		}
	}

	fn with_registration(control_tx: Queue<Bytes>, pending: OutgoingRegistration) -> Self {
		Self {
			control_tx,
			pending: Some(pending),
			updates: None,
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

			if let Some((request_id, len)) = pending.try_parse()? {
				let mut pending = self.pending.take().unwrap();
				let mut buf = std::mem::take(&mut pending.buf);
				let rest = buf.split_off(len);
				if pending.version == Version::Draft16 {
					self.updates = Some(UpdateRouting {
						shared: Arc::clone(&pending.shared),
						version: pending.version,
						request_id,
						buf: BytesMut::new(),
					});
				}
				pending.register(request_id);
				if !self.control_tx.push(buf.freeze()) {
					return Err(crate::Error::Closed);
				}
				if !rest.is_empty() {
					return self.push(rest.freeze());
				}
			}
		} else if let Some(updates) = &mut self.updates {
			updates.buf.extend_from_slice(&chunk);
			while let Some(message) = updates.next()? {
				if !self.control_tx.push(message) {
					return Err(crate::Error::Closed);
				}
			}
		} else if !self.control_tx.push(chunk) {
			return Err(crate::Error::Closed);
		}

		Ok(())
	}
}

/// Draft-16 answers a REQUEST_UPDATE on the control stream by the update's own Request
/// ID, which names no stream, so each update we send is recorded against the request it
/// rides, in [`Shared::updates`], before it leaves. Drafts 14 and 15 answer no update of
/// ours that anything reads.
struct UpdateRouting {
	shared: Arc<Shared>,
	version: Version,
	/// The request whose stream carries the updates.
	request_id: RequestId,
	buf: BytesMut,
}

impl UpdateRouting {
	/// The next whole message, recorded first when it is an update.
	fn next(&mut self) -> Result<Option<Bytes>, crate::Error> {
		let Some((type_id, len, mut body)) = whole_message(&self.buf, self.version) else {
			return Ok(None);
		};
		if type_id == ietf::SubscribeUpdate::ID {
			let own = RequestId::decode(&mut body, self.version)?;
			self.shared.route_update(own, self.request_id);
		}
		Ok(Some(self.buf.split_to(len).freeze()))
	}
}

/// The type, whole length, and body of the first message in `buf`, once all of it is there.
fn whole_message(buf: &[u8], version: Version) -> Option<(u64, usize, Decoder<'_>)> {
	let mut r = Decoder::new(buf, version.into());
	let type_id = r.varint().ok()?;
	let size = r.u16().ok()? as usize;
	let len = buf.len() - r.remaining() + size;
	let body = r.sub(size).ok()?;
	Some((type_id, len, body))
}

impl crate::transport::poll::SendStream for VirtualSendStream {
	type Error = crate::Error;
	type Group = crate::transport::poll::Ungrouped;

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

	fn set_priority(&mut self, _order: i64) {}

	fn set_group(&mut self, group: &Self::Group) {
		match *group {}
	}

	fn finish(&mut self) -> Result<(), Self::Error> {
		// Flush any remaining buffered data (e.g. if registration never completed).
		if let Some(pending) = self.pending.take()
			&& !pending.buf.is_empty()
		{
			let _ = self.control_tx.push(pending.buf.freeze());
		}
		if let Some(updates) = self.updates.take()
			&& !updates.buf.is_empty()
		{
			let _ = self.control_tx.push(updates.buf.freeze());
		}
		Ok(())
	}

	fn reset(&mut self, _code: u32) {}

	fn poll_closed(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
		Poll::Ready(Ok(()))
	}
}

// === Adapter Send/Recv Enums ===

/// A failure on an adapter stream. A real stream keeps its transport's error, so the negotiated
/// registry decodes a peer's reset rather than mistaking it for a lost connection.
/// Virtual streams have only local errors.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum AdapterError<E: crate::transport::Error> {
	#[error(transparent)]
	Real(E),
	#[error(transparent)]
	Virtual(crate::Error),
}

impl<E: crate::transport::Error> crate::transport::Error for AdapterError<E> {
	fn close_kind(&self) -> crate::transport::CloseKind {
		match self {
			Self::Real(err) => err.close_kind(),
			Self::Virtual(err) => err.close_kind(),
		}
	}

	fn session_error(&self) -> Option<(u32, String)> {
		match self {
			Self::Real(err) => err.session_error(),
			Self::Virtual(err) => err.session_error(),
		}
	}

	fn stream_error(&self) -> Option<u32> {
		match self {
			Self::Real(err) => err.stream_error(),
			Self::Virtual(err) => err.stream_error(),
		}
	}
}

pub(crate) enum AdapterSend {
	Real(crate::transport::boxed::SendStream),
	Virtual(VirtualSendStream),
}

impl crate::transport::poll::SendStream for AdapterSend {
	type Error = AdapterError<<crate::transport::boxed::SendStream as crate::transport::poll::SendStream>::Error>;
	type Group = <crate::transport::boxed::SendStream as crate::transport::poll::SendStream>::Group;

	fn poll_write(&mut self, cx: &mut Context<'_>, buf: &[u8]) -> Poll<Result<usize, Self::Error>> {
		match self {
			Self::Real(s) => s.poll_write(cx, buf).map_err(AdapterError::Real),
			Self::Virtual(s) => s.poll_write(cx, buf).map_err(AdapterError::Virtual),
		}
	}

	fn poll_write_buf<B: Buf>(&mut self, cx: &mut Context<'_>, buf: &mut B) -> Poll<Result<usize, Self::Error>> {
		match self {
			Self::Real(s) => s.poll_write_buf(cx, buf).map_err(AdapterError::Real),
			Self::Virtual(s) => s.poll_write_buf(cx, buf).map_err(AdapterError::Virtual),
		}
	}

	fn set_priority(&mut self, order: i64) {
		match self {
			Self::Real(s) => s.set_priority(order),
			Self::Virtual(s) => s.set_priority(order),
		}
	}

	fn set_group(&mut self, group: &Self::Group) {
		match self {
			Self::Real(s) => s.set_group(group),
			// Rides the control stream, which no group schedules.
			Self::Virtual(_) => {}
		}
	}

	fn finish(&mut self) -> Result<(), Self::Error> {
		match self {
			Self::Real(s) => s.finish().map_err(AdapterError::Real),
			Self::Virtual(s) => s.finish().map_err(AdapterError::Virtual),
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
			Self::Real(s) => s.poll_closed(cx).map_err(AdapterError::Real),
			Self::Virtual(s) => s.poll_closed(cx).map_err(AdapterError::Virtual),
		}
	}
}

#[cfg_attr(
	target_pointer_width = "64",
	expect(
		clippy::large_enum_variant,
		reason = "one per stream, so boxing a variant would add an allocation per stream to save bytes nothing holds in bulk"
	)
)]
pub(crate) enum AdapterRecv {
	Real(crate::transport::boxed::RecvStream),
	Virtual(crate::transport::poll::Buffered<VirtualRecvStream>),
}

impl crate::transport::poll::RecvStream for AdapterRecv {
	type Error = AdapterError<<crate::transport::boxed::RecvStream as crate::transport::poll::RecvStream>::Error>;
	type Lease<'a> = AdapterLease<'a>;

	fn poll_lease(&mut self, cx: &mut Context<'_>, want: usize) -> Poll<Result<AdapterLease<'_>, Self::Error>> {
		match self {
			Self::Real(s) => s
				.poll_lease(cx, want)
				.map(|r| r.map(AdapterLease::Real).map_err(AdapterError::Real)),
			Self::Virtual(s) => s
				.poll_lease(cx, want)
				.map(|r| r.map(AdapterLease::Virtual).map_err(AdapterError::Virtual)),
		}
	}

	fn set_priority(&mut self, priority: i64) {
		match self {
			Self::Real(s) => s.set_priority(priority),
			// Rides the control stream, which carries its credit.
			Self::Virtual(_) => {}
		}
	}

	fn stop(&mut self, code: u32) {
		match self {
			Self::Real(s) => s.stop(code),
			Self::Virtual(s) => s.stop(code),
		}
	}
}

/// A [`Lease`](crate::transport::poll::Lease) of an [`AdapterRecv`].
pub(crate) enum AdapterLease<'a> {
	Real(<crate::transport::boxed::RecvStream as crate::transport::poll::RecvStream>::Lease<'a>),
	Virtual(crate::transport::poll::BufferedLease<'a>),
}

impl Buf for AdapterLease<'_> {
	fn remaining(&self) -> usize {
		match self {
			Self::Real(l) => l.remaining(),
			Self::Virtual(l) => l.remaining(),
		}
	}

	fn chunk(&self) -> &[u8] {
		match self {
			Self::Real(l) => l.chunk(),
			Self::Virtual(l) => l.chunk(),
		}
	}

	fn advance(&mut self, n: usize) {
		match self {
			Self::Real(l) => l.advance(n),
			Self::Virtual(l) => l.advance(n),
		}
	}

	fn copy_to_bytes(&mut self, n: usize) -> Bytes {
		match self {
			Self::Real(l) => l.copy_to_bytes(n),
			Self::Virtual(l) => l.copy_to_bytes(n),
		}
	}
}

impl crate::transport::poll::Lease for AdapterLease<'_> {
	fn is_fin(&self) -> bool {
		match self {
			Self::Real(l) => l.is_fin(),
			Self::Virtual(l) => l.is_fin(),
		}
	}

	fn chunks(&self) -> impl Iterator<Item = &[u8]> {
		let (real, virt) = match self {
			Self::Real(l) => (Some(l.chunks()), None),
			Self::Virtual(l) => (None, Some(l.chunks())),
		};
		real.into_iter().flatten().chain(virt.into_iter().flatten())
	}

	fn release(self) {
		match self {
			Self::Real(l) => l.release(),
			Self::Virtual(l) => l.release(),
		}
	}
}

// === Control Stream Adapter ===

/// Which request a namespace names: an advertisement, by the side that made it, or the
/// peer's namespace subscription.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Direction {
	/// We advertised it to the peer, on a stream from `open_bi`.
	Outgoing,
	/// The peer advertised it to us, on a stream handed to `accept_bi`.
	Incoming,
	/// The peer subscribed to it with SUBSCRIBE_NAMESPACE, on a stream handed to
	/// `accept_bi`. Only draft-14's UNSUBSCRIBE_NAMESPACE names one.
	Subscribed,
}

/// Namespace → request_id reverse lookup for the v14/v15 messages that name a
/// namespace instead of a request id.
///
/// Direction is part of the key because a cluster mesh has both endpoints
/// advertising the same namespace to each other. One map would let the second
/// advertisement overwrite the first, and a terminal message would then tear
/// down the stream travelling the other way while its real target stayed open.
/// A subscription keeps a map of its own for the same reason: a peer may both
/// advertise a namespace and subscribe to it.
#[derive(Default)]
struct Namespaces {
	state: Mutex<NamespacesState>,
}

#[derive(Default)]
struct NamespacesState {
	outgoing: HashMap<PathOwned, NamespaceRequests>,
	incoming: HashMap<PathOwned, NamespaceRequests>,
	subscribed: HashMap<PathOwned, NamespaceRequests>,

	/// request_id → the namespace that request advertised, one entry per request
	/// including queued duplicates, so [`Namespaces::forget`] removes only that
	/// request from its namespace. The two directions share this map because
	/// moq-transport gives each peer its own request id space.
	by_request: HashMap<RequestId, NamespaceRegistration>,
}

struct NamespaceRequests {
	first: RequestId,
	last: RequestId,
}

struct NamespaceRegistration {
	direction: Direction,
	namespace: PathOwned,
	previous: Option<RequestId>,
	next: Option<RequestId>,
}

impl NamespacesState {
	fn map(&mut self, direction: Direction) -> &mut HashMap<PathOwned, NamespaceRequests> {
		match direction {
			Direction::Outgoing => &mut self.outgoing,
			Direction::Incoming => &mut self.incoming,
			Direction::Subscribed => &mut self.subscribed,
		}
	}
}

impl Namespaces {
	/// Record a request's namespace, leaving a live advertisement of the same name alone.
	///
	/// The first request in a direction owns the name until it ends. A peer that
	/// advertises the same namespace twice would otherwise move the name onto the second
	/// request, and the withdrawal it eventually names would close that one while the
	/// first stayed open, advertised for the life of the session.
	fn register(&self, direction: Direction, namespace: PathOwned, request_id: RequestId) {
		let mut state = self.state.lock().unwrap();
		if state.by_request.contains_key(&request_id) {
			return;
		}
		let previous = match state.map(direction).entry(namespace.clone()) {
			std::collections::hash_map::Entry::Occupied(mut entry) => {
				Some(std::mem::replace(&mut entry.get_mut().last, request_id))
			}
			std::collections::hash_map::Entry::Vacant(entry) => {
				entry.insert(NamespaceRequests {
					first: request_id,
					last: request_id,
				});
				None
			}
		};
		if let Some(previous) = previous {
			state.by_request.get_mut(&previous).unwrap().next = Some(request_id);
		}
		state.by_request.insert(
			request_id,
			NamespaceRegistration {
				direction,
				namespace,
				previous,
				next: None,
			},
		);
	}

	/// The namespace the peer's request `request_id` advertises, if it is a PUBLISH_NAMESPACE.
	fn incoming(&self, request_id: RequestId) -> Option<PathOwned> {
		let state = self.state.lock().unwrap();
		let registration = state.by_request.get(&request_id)?;
		(registration.direction == Direction::Incoming).then(|| registration.namespace.clone())
	}

	fn get(&self, direction: Direction, namespace: &PathOwned) -> Option<RequestId> {
		self.state
			.lock()
			.unwrap()
			.map(direction)
			.get(namespace)
			.map(|requests| requests.first)
	}

	/// Release whatever a finished request holds, so its namespace can be advertised again.
	fn forget(&self, request_id: RequestId) {
		let mut state = self.state.lock().unwrap();
		let Some(registration) = state.by_request.remove(&request_id) else {
			return;
		};

		// Link around the removed request without scanning a burst of duplicates.
		if let Some(previous) = registration.previous {
			state.by_request.get_mut(&previous).unwrap().next = registration.next;
		}
		if let Some(next) = registration.next {
			state.by_request.get_mut(&next).unwrap().previous = registration.previous;
		}
		let map = state.map(registration.direction);
		if registration.previous.is_none() && registration.next.is_none() {
			map.remove(&registration.namespace);
		} else {
			let requests = map.get_mut(&registration.namespace).unwrap();
			if requests.first == request_id {
				requests.first = registration.next.unwrap();
			}
			if requests.last == request_id {
				requests.last = registration.previous.unwrap();
			}
		}
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

	/// Draft-16: our REQUEST_UPDATEs' own Request IDs → the open request each updates, so
	/// the one answer each is owed reaches that request's stream. Bounded by what we sent,
	/// and an entry goes with its answer or its request. Locked after `streams`.
	updates: Mutex<HashMap<RequestId, RequestId>>,

	/// Namespace → request_id reverse lookup (for v14/v15 namespace-keyed messages).
	namespaces: Namespaces,

	/// The peer's advertisements held back behind a withdrawal still being handled.
	readvertised: Mutex<Readvertised>,
}

/// Keeps the peer's withdrawal and re-advertisement of a namespace in wire order.
///
/// Each request is served by its own handler, so a PUBLISH_NAMESPACE that follows a
/// PUBLISH_NAMESPACE_DONE for the same namespace could otherwise be handled first. It
/// would then read as a second advertisement of a namespace still advertised, and the
/// withdrawal as the end of the first, so the route never ends and starts again. The
/// re-advertisement reaches `accept_bi` only once the withdrawn request's handler drops
/// its stream, which it does after releasing the route. Bounded by the peer's requests.
#[derive(Default)]
struct Readvertised {
	namespaces: HashMap<PathOwned, Withdrawing>,
	/// Each withdrawn request a handler still holds, to the namespace it advertised.
	withdrawn: HashMap<RequestId, PathOwned>,
}

#[derive(Default)]
struct Withdrawing {
	/// Withdrawn requests whose handlers still hold their streams.
	handling: HashSet<RequestId>,
	/// The advertisements behind them, in wire order.
	held: VecDeque<HeldRequest>,
}

struct HeldRequest {
	request_id: RequestId,
	stream: (VirtualSendStream, VirtualRecvStream),
	/// Withdrawn while held, so the advertisements behind it wait for its handler too.
	withdrawn: bool,
}

impl Shared {
	/// Mint the local half of a request we're about to send. Registration waits
	/// until the first write reveals the request_id.
	fn open_outgoing(self: &Arc<Self>, version: Version) -> (VirtualSendStream, VirtualRecvStream) {
		let follow = Queue::new();
		let request_id = Arc::new(Mutex::new(None));
		let recv = VirtualRecvStream::new(Bytes::new(), follow.clone(), Arc::clone(self), Arc::clone(&request_id));
		let send = VirtualSendStream::with_registration(
			self.control.clone(),
			OutgoingRegistration {
				follow_tx: follow.writer(),
				shared: Arc::clone(self),
				version,
				buf: BytesMut::new(),
				request_id,
			},
		);
		(send, recv)
	}

	/// Register the peer's new request and queue its stream for accept_bi.
	fn open_incoming(
		self: &Arc<Self>,
		request_id: RequestId,
		raw: Bytes,
		permit: Option<RequestPermit>,
	) -> Result<(), Error> {
		let follow = Queue::new();
		let mut recv = VirtualRecvStream::new(
			raw,
			follow.clone(),
			Arc::clone(self),
			Arc::new(Mutex::new(Some(request_id))),
		);
		recv._permit = permit;
		let send = VirtualSendStream::new(self.control.clone());
		self.streams.lock().unwrap().insert(request_id, follow.writer());
		if let Some(namespace) = self.namespaces.incoming(request_id) {
			let mut readvertised = self.readvertised.lock().unwrap();
			if let Some(withdrawing) = readvertised.namespaces.get_mut(&namespace) {
				withdrawing.held.push_back(HeldRequest {
					request_id,
					stream: (send, recv),
					withdrawn: false,
				});
				return Ok(());
			}
		}
		if !self.incoming.push((send, recv)) {
			return Err(Error::Closed);
		}
		Ok(())
	}

	/// Deliver a message to an open virtual stream, dropping it if the stream is gone.
	/// An answer to one of our updates goes to the request it updated.
	/// Returns whether a stream took it.
	fn push(&self, request_id: RequestId, raw: Bytes) -> bool {
		let streams = self.streams.lock().unwrap();
		let request_id = self.updates.lock().unwrap().remove(&request_id).unwrap_or(request_id);
		match streams.get(&request_id) {
			Some(tx) => tx.push(raw),
			None => false,
		}
	}

	/// Deliver a final message and FIN the stream: the removed writer drops after it.
	/// A refusal of one of our updates is only its answer, which leaves the request open.
	fn close(&self, request_id: RequestId, raw: Bytes) {
		if self.updates.lock().unwrap().contains_key(&request_id) {
			self.push(request_id, raw);
			return;
		}
		let tx = self.streams.lock().unwrap().remove(&request_id);
		let namespace = self.namespaces.incoming(request_id);
		self.namespaces.forget(request_id);
		let Some(tx) = tx else { return };
		let Some(namespace) = namespace else {
			tx.push(raw);
			return;
		};

		// Under the lock, so a handler dropping its stream either refuses the push or
		// finds the withdrawal recorded.
		let mut readvertised = self.readvertised.lock().unwrap();
		if !tx.push(raw) {
			return;
		}
		let withdrawing = readvertised.namespaces.entry(namespace.clone()).or_default();
		match withdrawing.held.iter_mut().find(|held| held.request_id == request_id) {
			Some(held) => held.withdrawn = true,
			None => {
				withdrawing.handling.insert(request_id);
				readvertised.withdrawn.insert(request_id, namespace);
			}
		}
	}

	/// Expect the answer to our update `own` on `request_id`'s stream, while it is open.
	fn route_update(&self, own: RequestId, request_id: RequestId) {
		let streams = self.streams.lock().unwrap();
		if streams.contains_key(&request_id) {
			self.updates.lock().unwrap().insert(own, request_id);
		}
	}

	/// Drop every routing entry a request owns, whether or not its stream is still around.
	fn forget(&self, request_id: RequestId) {
		let mut streams = self.streams.lock().unwrap();
		streams.remove(&request_id);
		let mut updates = self.updates.lock().unwrap();
		if !updates.is_empty() {
			updates.retain(|_, updated| *updated != request_id);
		}
		drop((updates, streams));
		self.namespaces.forget(request_id);
		self.handled(request_id);
	}

	/// A withdrawn request's handler is done with it: once none of the namespace's
	/// withdrawals is still being handled, hand over what was held behind them, up to and
	/// including the next one withdrawn.
	fn handled(&self, request_id: RequestId) {
		let mut readvertised = self.readvertised.lock().unwrap();
		let Some(namespace) = readvertised.withdrawn.remove(&request_id) else {
			return;
		};
		let Readvertised { namespaces, withdrawn } = &mut *readvertised;
		let Some(withdrawing) = namespaces.get_mut(&namespace) else {
			return;
		};
		withdrawing.handling.remove(&request_id);
		while withdrawing.handling.is_empty() {
			let Some(held) = withdrawing.held.pop_front() else {
				namespaces.remove(&namespace);
				return;
			};
			if held.withdrawn {
				withdrawing.handling.insert(held.request_id);
				withdrawn.insert(held.request_id, namespace.clone());
			}
			// A closed queue means the session is gone, and the stream with it.
			let _ = self.incoming.push(held.stream);
		}
	}
}

#[derive(Clone)]
pub(crate) struct ControlStreamAdapter {
	stats: crate::stats::Session,
	inner: Transport,
	shared: Arc<Shared>,
	control: Control,
	version: Version,
	// Bridges Context-based polls onto the kio queues; empty on clone.
	park: kio::Park,
}

impl ControlStreamAdapter {
	pub fn new(inner: Transport, control: Control, version: Version) -> Self {
		Self {
			stats: Default::default(),
			inner,
			shared: Arc::new(Shared::default()),
			control,
			version,
			park: kio::Park::default(),
		}
	}

	pub(super) fn with_stats(mut self, stats: &crate::stats::Session) -> Self {
		self.stats = stats.clone();
		self
	}

	/// Open a real (non-virtual) bidi stream, bypassing control stream multiplexing.
	/// Used for v16 SubscribeNamespace which moved to its own bidi stream.
	pub async fn open_native_bi(&mut self) -> Result<(AdapterSend, AdapterRecv), crate::Error> {
		let (send, recv) = self
			.inner
			.open_bi(Default::default())
			.await
			.map_err(|_| crate::Error::Closed)?;
		Ok((AdapterSend::Real(send), AdapterRecv::Real(recv)))
	}

	/// Run the control stream read + write tasks.
	/// This reads from the control stream and routes messages to virtual streams,
	/// and also drains the write queue to the control stream writer. A decoded
	/// GOAWAY is surfaced through `goaway` so [`crate::Session::recv_goaway`] resolves
	/// instead of the session tearing down.
	pub async fn run(
		&self,
		reader: Reader<crate::transport::boxed::RecvStream, Version>,
		writer: Writer<crate::transport::boxed::SendStream, Version>,
		goaway: crate::goaway::Protocol,
	) -> Result<(), Error> {
		let res = {
			let mut read = std::pin::pin!(self.run_read(reader, goaway));
			let mut write = std::pin::pin!(self.run_write(writer));
			kio::wait(|waiter| {
				if let Poll::Ready(res) = waiter.poll_future(read.as_mut()) {
					return Poll::Ready(res);
				}
				// Queued before the write drains, so a grant goes out in the same turn.
				while let Poll::Ready(max) = self.control.poll_grant(waiter) {
					self.send_max_request_id(max);
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
		let msg = crate::ietf::GoAway {
			new_session_uri: std::borrow::Cow::Borrowed(uri),
			timeout: timeout_ms,
			request_id: None,
		};

		if let Err(err) = self.queue_message(&msg, version) {
			tracing::warn!(%err, "failed to encode goaway");
		}
	}

	/// Refuse an update no request here can apply, naming its own Request ID.
	fn refuse_update(&self, request_id: RequestId) {
		if self.version == Version::Draft14 {
			return;
		}
		let msg = ietf::RequestError {
			request_id: Some(request_id),
			error_code: super::error::request::to_code(
				&crate::Error::Unsupported,
				super::error::request::Kind::Subscribe,
				self.version,
			),
			reason_phrase: "update not supported".into(),
			retry_interval: 0,
		};
		if let Err(err) = self.queue_message(&msg, self.version) {
			tracing::warn!(%err, "failed to encode REQUEST_ERROR");
		}
	}

	/// Queue a MAX_REQUEST_ID granting the peer room up to `max`.
	fn send_max_request_id(&self, max: RequestId) {
		if let Err(err) = self.queue_message(&ietf::MaxRequestId { request_id: max }, self.version) {
			tracing::warn!(%err, "failed to encode MAX_REQUEST_ID");
		}
	}

	fn queue_message<T: Message>(&self, msg: &T, version: Version) -> Result<(), Error> {
		use crate::coding::Encode;
		let mut raw = Vec::new();
		let mut encoder = Encoder::new(&mut raw, version.into());
		encoder.varint(T::ID)?;
		msg.encode(&mut encoder, version)?;
		let selector = crate::coding::varint::size(T::ID, version.into()).map_err(crate::coding::EncodeError::from)?;
		self.stats
			.control(super::message::control(T::ID, version), raw.len() - selector, false);
		if !self.shared.control.push(raw.into()) {
			tracing::debug!("control stream closed; message not sent");
		}
		Ok(())
	}

	/// Writer task: drains the queue and writes to the control stream. The peer's
	/// STOP_SENDING, seen while idle or as a failed write, is a protocol violation.
	async fn run_write(&self, mut writer: Writer<crate::transport::boxed::SendStream, Version>) -> Result<(), Error> {
		loop {
			let msg = kio::wait(|waiter| {
				// Never finished, so any end is the peer's or the session's.
				if let Poll::Ready(res) = writer.poll_closed(&mut waiter.context()) {
					let err = res
						.err()
						.map_or(Error::ProtocolViolation, super::session::control_stream_error);
					return Poll::Ready(Err(err));
				}
				self.shared.control.poll_pop(waiter).map(Ok)
			})
			.await?;
			let Some(msg) = msg else { break };
			let mut buf = std::io::Cursor::new(msg);
			writer
				.write_all(&mut buf)
				.await
				.map_err(super::session::control_stream_error)?;
		}
		Ok(())
	}

	/// Dispatcher loop that reads control stream messages and routes them.
	async fn run_read(
		&self,
		mut reader: Reader<crate::transport::boxed::RecvStream, Version>,
		goaway: crate::goaway::Protocol,
	) -> Result<(), Error> {
		loop {
			let type_id = match reader
				.varint_maybe()
				.await
				.map_err(super::session::control_stream_error)?
			{
				Some(id) => id,
				None => return Err(Error::ProtocolViolation),
			};

			let body = reader
				.decode::<ietf::Body>()
				.await
				.map_err(super::session::control_stream_error)?
				.0;

			self.stats
				.control(super::message::control(type_id, self.version), body.len() + 2, true);

			// Reconstruct raw message bytes: [type_id][size][body]
			let raw = encode_raw(type_id, &body, self.version);

			// Classify and route
			match classify(type_id, &body, self.version, &self.shared.namespaces)? {
				Route::NewRequest(request_id) => {
					let permit = self.control.accept(request_id)?;
					self.shared.open_incoming(request_id, raw, permit)?
				}
				Route::Response(request_id) => {
					self.shared.push(request_id, raw);
				}
				Route::FollowUp { own, target } => {
					// SUBSCRIBE_UPDATE takes a request ID of its own (draft-14 section 9.1),
					// done as soon as it is delivered, so it is granted straight back.
					drop(self.control.accept(own)?);
					// An update whose request is gone, or rides a stream of its own like draft 16's
					// SUBSCRIBE_NAMESPACE, is still owed its one answer.
					if !self.shared.push(target, raw) {
						self.refuse_update(own);
					}
				}
				Route::CloseStream(request_id) => self.shared.close(request_id, raw),
				Route::MaxRequestId(max) => self.control.max_request_id(max),
				Route::Ignore => {}
				Route::GoAway => {
					let mut data = Decoder::new(&body, self.version.into());
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
				namespaces.register(Direction::Incoming, ns, id);
			}
			Ok(Route::NewRequest(id))
		}
		ietf::TrackStatus::ID => {
			let id = decode_request_id(body, version)?;
			Ok(Route::NewRequest(id))
		}
		// SubscribeNamespace on control stream (v14/v15 only)
		ietf::SubscribeNamespaceLegacy::ID => match version {
			// Draft-14's UNSUBSCRIBE_NAMESPACE names the prefix rather than the request.
			Version::Draft14 => {
				let mut r = Decoder::new(body, version.into());
				let id = RequestId::decode(&mut r, version)?;
				if let Ok(prefix) = crate::ietf::namespace::decode_namespace(&mut r) {
					namespaces.register(Direction::Subscribed, prefix.into_owned(), id);
				}
				Ok(Route::NewRequest(id))
			}
			Version::Draft15 => {
				let id = decode_request_id(body, version)?;
				Ok(Route::NewRequest(id))
			}
			_ => Err(Error::UnexpectedMessage),
		},

		// SUBSCRIBE_TRACKS (draft-18+, #1542): the half of the SUBSCRIBE_NAMESPACE split
		// that subscribes to all tracks under a prefix. We don't implement PUBLISH
		// replication, so this is impossible to honor. Reject loudly.
		ietf::SubscribeTracks::ID => {
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
		// TRACK_STATUS_OK and TRACK_STATUS_ERROR (v14 only): the one answer the request gets.
		ietf::TrackStatusOk::ID_14 | ietf::TRACK_STATUS_ERROR_14 => match version {
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

		// Follow-up messages: route to the request being updated, not the update's own id.
		ietf::SubscribeUpdate::ID => {
			let own = decode_request_id(body, version)?;
			let target = decode_update_target(body, version)?;
			Ok(Route::FollowUp { own, target })
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
				match lookup_namespace_request_id(body, version, namespaces, Direction::Incoming)? {
					Some(id) => Ok(Route::CloseStream(id)),
					None => Ok(Route::Ignore),
				}
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
				match lookup_namespace_request_id(body, version, namespaces, Direction::Outgoing)? {
					Some(id) => Ok(Route::CloseStream(id)),
					None => Ok(Route::Ignore),
				}
			}
			_ => Err(Error::UnexpectedMessage),
		},
		ietf::UnsubscribeNamespace::ID => match version {
			Version::Draft14 => match lookup_namespace_request_id(body, version, namespaces, Direction::Subscribed)? {
				Some(id) => Ok(Route::CloseStream(id)),
				None => Ok(Route::Ignore),
			},
			Version::Draft15 => {
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
		// The peer reached the MAX_REQUEST_ID we advertised and waits for a grant, which
		// follows on its own as its requests close.
		ietf::RequestsBlocked::ID => Ok(Route::Ignore),

		// Terminal
		ietf::GoAway::ID => Ok(Route::GoAway),

		_ => Err(Error::UnexpectedMessage),
	}
}

/// Decode the namespace from a v14/v15 namespace-keyed message body and look up the
/// request_id of the advertisement it terminates, travelling in the given direction.
///
/// `None` when nothing holds that name: the request it named is already gone. There is
/// nothing left to close, so the message is dropped rather than ending the session.
fn lookup_namespace_request_id(
	body: &Bytes,
	version: Version,
	namespaces: &Namespaces,
	direction: Direction,
) -> Result<Option<RequestId>, Error> {
	let mut r = Decoder::new(body, version.into());
	let ns = crate::ietf::namespace::decode_namespace(&mut r)?;
	Ok(namespaces.get(direction, &ns))
}

impl crate::transport::poll::Session for ControlStreamAdapter {
	type SendStream = AdapterSend;
	type RecvStream = AdapterRecv;
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
			Some((send, recv)) => Poll::Ready(Ok((
				AdapterSend::Virtual(send),
				AdapterRecv::Virtual(crate::transport::poll::Buffered::new(recv)),
			))),
			None => Poll::Ready(Err(crate::Error::Closed)),
		}
	}

	fn poll_open_bi(
		&mut self,
		_cx: &mut Context<'_>,
		// Rides the control stream, which no group schedules.
		_: crate::transport::poll::SendOptions<'_, <Self::SendStream as crate::transport::poll::SendStream>::Group>,
	) -> Poll<Result<(Self::SendStream, Self::RecvStream), Self::Error>> {
		let (send, recv) = self.shared.open_outgoing(self.version);
		Poll::Ready(Ok((
			AdapterSend::Virtual(send),
			AdapterRecv::Virtual(crate::transport::poll::Buffered::new(recv)),
		)))
	}

	fn poll_open_uni(
		&mut self,
		cx: &mut Context<'_>,
		options: crate::transport::poll::SendOptions<
			'_,
			<Self::SendStream as crate::transport::poll::SendStream>::Group,
		>,
	) -> Poll<Result<Self::SendStream, Self::Error>> {
		self.inner
			.poll_open_uni(cx, options)
			.map(|r| r.map(AdapterSend::Real).map_err(|_| crate::Error::Closed))
	}

	fn poll_accept_uni(&mut self, cx: &mut Context<'_>) -> Poll<Result<Self::RecvStream, Self::Error>> {
		self.inner
			.poll_accept_uni(cx)
			.map(|r| r.map(AdapterRecv::Real).map_err(|_| crate::Error::Closed))
	}

	fn send_group(&mut self, priority: i64) -> Option<<Self::SendStream as crate::transport::poll::SendStream>::Group> {
		self.inner.send_group(priority)
	}

	fn send_datagram(
		&mut self,
		payload: Bytes,
		options: crate::transport::poll::SendOptions<
			'_,
			<Self::SendStream as crate::transport::poll::SendStream>::Group,
		>,
	) -> Result<(), Self::Error> {
		self.inner
			.send_datagram(payload, options)
			.map_err(|_| crate::Error::Closed)
	}

	fn poll_recv_datagram(&mut self, cx: &mut Context<'_>) -> Poll<Result<Bytes, Self::Error>> {
		self.inner
			.poll_recv_datagram(cx)
			.map(|r| r.map_err(|_| crate::Error::Closed))
	}

	fn max_datagram_size(&self) -> usize {
		self.inner.max_datagram_size()
	}

	fn binding(&self) -> crate::transport::Binding {
		self.inner.binding()
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

	fn stats(&self) -> impl crate::transport::Stats {
		self.inner.stats()
	}
}

// === Message Classification ===

#[derive(Debug)]
enum Route {
	NewRequest(RequestId),
	Response(RequestId),
	/// An update: its own request ID counts against the window, and it goes to `target`.
	FollowUp {
		own: RequestId,
		target: RequestId,
	},
	CloseStream(RequestId),
	MaxRequestId(RequestId),
	/// Nothing to route: the message named a request that is already gone.
	Ignore,
	GoAway,
}

/// Encode raw message bytes as [type_id varint][size u16][body].
fn encode_raw(type_id: u64, body: &Bytes, version: Version) -> Bytes {
	let mut buf = Vec::new();
	let mut w = Encoder::new(&mut buf, version.into());
	w.varint(type_id).expect("type_id was read from the same wire");
	w.u16(u16::try_from(body.len()).expect("body was read with a u16 size"));
	w.slice(body);
	buf.into()
}

/// Decode just the request_id from the beginning of a message body.
fn decode_request_id(body: &Bytes, version: Version) -> Result<RequestId, Error> {
	let (request_id, _) = RequestId::decode_slice(body, version)?;
	Ok(request_id)
}

/// The request a SUBSCRIBE_UPDATE or REQUEST_UPDATE applies to.
///
/// Drafts 14 and 15 name it in Subscription Request ID, and draft 16 in Existing
/// Request ID. Both are the second field; the first is the update's own new
/// Request ID. Later drafts send updates on the request's own stream, not here.
fn decode_update_target(body: &Bytes, version: Version) -> Result<RequestId, Error> {
	match version {
		Version::Draft14 | Version::Draft15 | Version::Draft16 => {
			let mut r = Decoder::new(body, version.into());
			let _own = RequestId::decode(&mut r, version)?;
			Ok(RequestId::decode(&mut r, version)?)
		}
		_ => Err(Error::UnexpectedMessage),
	}
}

/// Decode request_id for response messages that have Option<RequestId> in v14-16.
fn decode_response_request_id(body: &Bytes, version: Version) -> Result<RequestId, Error> {
	// In v14-16, response messages always have request_id present
	decode_request_id(body, version)
}

/// Decode the namespace from a PublishNamespace message body (after the request_id).
fn decode_publish_namespace_body(body: &Bytes, version: Version) -> Result<PathOwned, Error> {
	let mut r = Decoder::new(body, version.into());
	// Skip request_id
	let _request_id = RequestId::decode(&mut r, version)?;
	// v17 has required_request_id_delta
	if version == Version::Draft17 {
		r.varint()?;
	}
	let ns = crate::ietf::namespace::decode_namespace(&mut r)?;
	Ok(ns.into_owned())
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::coding::Encode;
	use crate::transport::poll::{RecvStream as _, SendStream as _};
	use futures::FutureExt as _;

	fn make_body_with_request_id(id: u64, version: Version) -> Bytes {
		RequestId(id).encode_bytes(version).unwrap()
	}

	/// Classify against an empty namespace map, for the messages that don't use it.
	fn classify_msg(version: Version, type_id: u64, body: &Bytes) -> Result<Route, Error> {
		classify(type_id, body, version, &Namespaces::default())
	}

	/// A receive stream with no request behind it, for exercising the reader alone.
	fn detached_recv(initial: Bytes, rx: Queue<Bytes>) -> crate::transport::poll::Buffered<VirtualRecvStream> {
		crate::transport::poll::Buffered::new(VirtualRecvStream::new(
			initial,
			rx,
			Arc::new(Shared::default()),
			Arc::new(Mutex::new(None)),
		))
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

	/// Drafts 14 and 15 name the subscription in the second field; draft 16 names
	/// the existing request there. The first field is the update's own Request ID,
	/// so a body that carries only that ID cannot tell the two apart.
	#[moq_clock::test]
	async fn test_subscribe_update_reaches_its_target() {
		let update_id = RequestId(10);
		let target_id = RequestId(4);

		for version in [Version::Draft14, Version::Draft15, Version::Draft16] {
			let update = ietf::SubscribeUpdate {
				max_age: None,
				new_group: None,
				request_id: update_id,
				subscription_request_id: Some(target_id),
				start_location: ietf::Location { group: 1, object: 2 },
				end_group: 100,
				subscriber_priority: Some(200),
				forward: Some(false),
				filter: Some(ietf::Filter::NextObject),
				authorization_token: None,
				fill: None,
			};
			let body = encode_body(&update, version);
			let raw = encode_raw(ietf::SubscribeUpdate::ID, &body, version);
			let Route::FollowUp { own, target: routed } =
				classify_msg(version, ietf::SubscribeUpdate::ID, &body).unwrap()
			else {
				panic!("{version} did not route the update as a follow-up");
			};
			assert_eq!(own, update_id, "{version}");

			let shared = Arc::new(Shared::default());
			shared
				.open_incoming(target_id, Bytes::from_static(b"request"), None)
				.unwrap();
			let (_, mut recv) = shared.incoming.pop().await.unwrap();
			assert_eq!(
				read_virtual(&mut recv).await.unwrap(),
				Some(Bytes::from_static(b"request"))
			);

			assert_eq!(routed, target_id, "{version}: update {update_id} routed to {routed}");
			shared.push(routed, raw.clone());
			assert_eq!(
				read_virtual(&mut recv).await.unwrap(),
				Some(raw),
				"{version}: update {update_id} did not reach {target_id}"
			);
		}
	}

	/// Draft-16 answers our REQUEST_UPDATE by the update's own Request ID. The answer
	/// reaches the request it updated, a refusal leaves that request open, and an update
	/// written in pieces is recorded all the same.
	#[moq_clock::test]
	async fn our_update_answers_reach_their_request() {
		const VERSION: Version = Version::Draft16;
		let shared = Arc::new(Shared::default());
		let (mut send, mut recv) = shared.open_outgoing(VERSION);
		let update = |request_id| {
			encode_msg(
				&ietf::PublishNamespaceUpdate {
					request_id: RequestId(request_id),
					namespace_request_id: Some(RequestId(4)),
					hops: None,
					cost: None,
					authorization_token: None,
				},
				VERSION,
			)
		};

		// The request and the first byte of its update in one write, the rest in another.
		let first = update(6);
		let request = encode_msg(&publish_namespace(RequestId(4), "ns"), VERSION);
		send.write_chunk([request.clone(), first.slice(..1)].concat().into())
			.await
			.unwrap();
		send.write_chunk(first.slice(1..)).await.unwrap();
		assert_eq!(shared.control.pop().await.unwrap(), request);
		assert_eq!(shared.control.pop().await.unwrap(), first);

		let ok = ietf::RequestOk {
			largest: None,
			request_id: Some(RequestId(6)),
			active: None,
		};
		assert!(matches!(
			classify_msg(VERSION, ietf::RequestOk::ID, &encode_body(&ok, VERSION)).unwrap(),
			Route::Response(RequestId(6))
		));
		let ok = encode_msg(&ok, VERSION);
		assert!(shared.push(RequestId(6), ok.clone()), "the answer found no stream");
		assert_eq!(read_virtual(&mut recv).await.unwrap(), Some(ok.clone()));
		assert!(!shared.push(RequestId(6), ok), "a second answer was routed");

		send.write_chunk(update(8)).await.unwrap();
		assert_eq!(shared.control.pop().await.unwrap(), update(8));
		let error = encode_msg(
			&ietf::RequestError {
				request_id: Some(RequestId(8)),
				error_code: 0,
				reason_phrase: "refused".into(),
				retry_interval: 0,
			},
			VERSION,
		);
		shared.close(RequestId(8), error.clone());
		assert_eq!(read_virtual(&mut recv).await.unwrap(), Some(error));
		assert!(
			shared.push(RequestId(4), Bytes::from_static(b"later")),
			"the refusal closed the request"
		);

		// An update outstanding when its request goes leaves nothing behind.
		send.write_chunk(update(10)).await.unwrap();
		assert_eq!(shared.updates.lock().unwrap().len(), 1);
		drop(recv);
		assert!(shared.updates.lock().unwrap().is_empty());
	}

	/// Before draft-16 an answer to our update names nothing we route.
	#[moq_clock::test]
	async fn draft_15_update_answers_are_not_routed() {
		const VERSION: Version = Version::Draft15;
		let shared = Arc::new(Shared::default());
		let (mut send, _recv) = shared.open_outgoing(VERSION);
		send.write_chunk(encode_msg(&publish_namespace(RequestId(4), "ns"), VERSION))
			.await
			.unwrap();
		// A SUBSCRIBE_UPDATE's header and its own Request ID 6, all that routing reads.
		send.write_chunk(Bytes::from_static(&[0x02, 0x00, 0x01, 0x06]))
			.await
			.unwrap();
		assert!(!shared.push(RequestId(6), Bytes::from_static(b"ok")));
		assert!(shared.updates.lock().unwrap().is_empty());
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
		let raw = encode_raw(0x03, &body, version);

		// Decode the raw bytes
		let mut r = Decoder::new(&raw, version.into());
		assert_eq!(r.varint().unwrap(), 0x03);
		assert_eq!(r.u16().unwrap(), 5);
		assert_eq!(r.rest(), b"hello");
	}

	#[moq_clock::test]
	async fn test_virtual_recv_stream_reads_initial_then_followup() {
		let initial = Bytes::from_static(b"initial");
		let follow = Queue::new();
		let tx = follow.writer();
		let mut stream = detached_recv(initial, follow);

		// Read initial data
		let chunk = stream.read_chunk(32).await.unwrap().unwrap();
		assert_eq!(&chunk[..], b"initial");

		// Send follow-up
		tx.push(Bytes::from_static(b"followup"));
		let chunk = stream.read_chunk(32).await.unwrap().unwrap();
		assert_eq!(&chunk[..], b"followup");

		// Drop the writer → FIN
		drop(tx);
		let result = stream.read_chunk(32).await.unwrap();
		assert_eq!(result, None);
	}

	#[moq_clock::test]
	async fn test_virtual_recv_stream_partial_reads() {
		let initial = Bytes::from_static(b"hello world");
		let mut stream = detached_recv(initial, Queue::new());

		// Read small chunks
		let chunk = stream.read_chunk(5).await.unwrap().unwrap();
		assert_eq!(&chunk[..], b"hello");

		let chunk = stream.read_chunk(5).await.unwrap().unwrap();
		assert_eq!(&chunk[..], b" worl");

		let chunk = stream.read_chunk(1).await.unwrap().unwrap();
		assert_eq!(&chunk[..], b"d");
	}

	#[moq_clock::test]
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
		let stream = detached_recv(Bytes::new(), follow.clone());
		drop(stream);
		assert!(!follow.push(Bytes::from_static(b"late")));
	}

	#[moq_clock::test]
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
		let mut buf = Vec::new();
		msg.encode_msg(&mut Encoder::new(&mut buf, version.into()), version)
			.unwrap();
		bytes::Bytes::from(buf)
	}

	/// Encode a full control message: [type_id][size][body].
	fn encode_msg<M: Message>(msg: &M, version: Version) -> Bytes {
		let body = encode_body(msg, version);
		encode_raw(M::ID, &body, version)
	}

	fn publish_namespace(request_id: RequestId, namespace: &str) -> ietf::PublishNamespace<'_> {
		ietf::PublishNamespace {
			request_id,
			track_namespace: crate::Path::new(namespace),
			params: Default::default(),
			authorization_token: None,
		}
	}

	/// Advertise `namespace` to the peer on `request_id`, as `open_bi` + a write would.
	async fn advertise(
		shared: &Arc<Shared>,
		namespace: &str,
		version: Version,
		request_id: RequestId,
	) -> VirtualRecvStream {
		let (mut send, recv) = shared.open_outgoing(version);
		send.write_chunk(encode_msg(&publish_namespace(request_id, namespace), version))
			.await
			.unwrap();
		recv
	}

	/// Receive the peer's advertisement of `namespace` on `request_id`, as the read loop would.
	async fn receive(
		shared: &Arc<Shared>,
		namespace: &str,
		version: Version,
		request_id: RequestId,
	) -> VirtualRecvStream {
		let msg = publish_namespace(request_id, namespace);
		let route = classify(
			ietf::PublishNamespace::ID,
			&encode_body(&msg, version),
			version,
			&shared.namespaces,
		)
		.unwrap();
		assert!(matches!(route, Route::NewRequest(id) if id == request_id));
		shared
			.open_incoming(request_id, encode_msg(&msg, version), None)
			.unwrap();

		let (_, mut recv) = shared.incoming.pop().await.unwrap();

		// Drain the advertisement the stream opened with, so both halves are quiescent
		// and a later read only sees what the terminal message routes.
		assert_eq!(read_virtual(&mut recv).await.unwrap(), Some(encode_msg(&msg, version)));
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
				let theirs = receive(&shared, namespace, version, RequestId(7)).await;
				(advertise(&shared, namespace, version, RequestId(4)).await, theirs)
			}
			Direction::Incoming => {
				let ours = advertise(&shared, namespace, version, RequestId(4)).await;
				(ours, receive(&shared, namespace, version, RequestId(7)).await)
			}
			Direction::Subscribed => unreachable!("a mesh only advertises"),
		};

		(shared, ours, theirs)
	}

	/// The next chunk of a virtual stream, read without a lease.
	fn read_virtual(stream: &mut VirtualRecvStream) -> impl Future<Output = Result<Option<Bytes>, crate::Error>> + '_ {
		std::future::poll_fn(|cx| crate::transport::poll::Chunks::poll_read_chunk(stream, cx, usize::MAX))
	}

	/// Read until the stream FINs, returning whether it did so within `limit` reads.
	async fn drained(stream: &mut VirtualRecvStream, limit: usize) -> bool {
		for _ in 0..limit {
			if read_virtual(stream).await.unwrap().is_none() {
				return true;
			}
		}
		false
	}

	#[test]
	fn test_namespace_reverse_lookup_v14() {
		let namespaces = Namespaces::default();
		namespaces.register(Direction::Incoming, crate::Path::new("test/ns"), RequestId(42));

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

	#[moq_clock::test]
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
		assert!(read_virtual(&mut ours).now_or_never().is_none());
	}

	#[moq_clock::test]
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
		assert!(read_virtual(&mut theirs).now_or_never().is_none());
	}

	/// Classify the peer withdrawing its own advertisement of `namespace`.
	fn done(shared: &Arc<Shared>, namespace: &str, version: Version) -> Route {
		let msg = ietf::PublishNamespaceDone {
			track_namespace: crate::Path::new(namespace),
			request_id: RequestId(0),
		};
		classify(
			ietf::PublishNamespaceDone::ID,
			&encode_body(&msg, version),
			version,
			&shared.namespaces,
		)
		.unwrap()
	}

	/// Classify the peer rejecting our advertisement of `namespace`.
	fn cancel(shared: &Arc<Shared>, namespace: &str, version: Version) -> Route {
		let msg = ietf::PublishNamespaceCancel {
			track_namespace: crate::Path::new(namespace),
			request_id: RequestId(0),
			error_code: 0,
			reason_phrase: "".into(),
		};
		classify(
			ietf::PublishNamespaceCancel::ID,
			&encode_body(&msg, version),
			version,
			&shared.namespaces,
		)
		.unwrap()
	}

	#[moq_clock::test]
	async fn test_duplicate_publish_namespace_keeps_the_first() {
		// The peer advertises the same namespace twice. The session refuses the second, so
		// the first still owns the name and its withdrawal must reach the first's stream.
		let version = Version::Draft14;
		let shared = Arc::new(Shared::default());
		let first = receive(&shared, "cluster/ns", version, RequestId(7)).await;
		let duplicate = receive(&shared, "cluster/ns", version, RequestId(11)).await;

		assert!(matches!(
			done(&shared, "cluster/ns", version),
			Route::CloseStream(RequestId(7))
		));

		// Tearing down the refused duplicate releases only what it owns, which is nothing.
		drop(duplicate);
		assert!(matches!(
			done(&shared, "cluster/ns", version),
			Route::CloseStream(RequestId(7))
		));

		// The winner going away is what frees the name.
		drop(first);
		assert!(matches!(done(&shared, "cluster/ns", version), Route::Ignore));
	}

	#[moq_clock::test]
	async fn test_withdrawal_releases_the_namespace() {
		// A namespace the peer withdraws must be advertisable again on a new request.
		let version = Version::Draft14;
		let shared = Arc::new(Shared::default());
		let mut theirs = receive(&shared, "cluster/ns", version, RequestId(7)).await;

		let msg = ietf::PublishNamespaceDone {
			track_namespace: crate::Path::new("cluster/ns"),
			request_id: RequestId(0),
		};
		shared.close(RequestId(7), encode_msg(&msg, version));
		assert!(drained(&mut theirs, 4).await);
		assert!(!shared.streams.lock().unwrap().contains_key(&RequestId(7)));
		drop(theirs);

		let _again = receive(&shared, "cluster/ns", version, RequestId(11)).await;
		assert!(matches!(
			done(&shared, "cluster/ns", version),
			Route::CloseStream(RequestId(11))
		));
	}

	/// The peer's next request on `namespace`, advertising it, as the read loop routes it.
	fn readvertise(shared: &Arc<Shared>, namespace: &str, version: Version, request_id: RequestId) {
		let msg = publish_namespace(request_id, namespace);
		let route = classify(
			ietf::PublishNamespace::ID,
			&encode_body(&msg, version),
			version,
			&shared.namespaces,
		)
		.unwrap();
		assert!(matches!(route, Route::NewRequest(id) if id == request_id));
		shared
			.open_incoming(request_id, encode_msg(&msg, version), None)
			.unwrap();
	}

	/// The peer withdrawing `namespace` on `request_id`, as the read loop routes it.
	fn withdraw(shared: &Arc<Shared>, namespace: &str, version: Version, request_id: RequestId) {
		let msg = ietf::PublishNamespaceDone {
			track_namespace: crate::Path::new(namespace),
			request_id,
		};
		let route = classify(
			ietf::PublishNamespaceDone::ID,
			&encode_body(&msg, version),
			version,
			&shared.namespaces,
		)
		.unwrap();
		assert!(matches!(route, Route::CloseStream(id) if id == request_id));
		shared.close(request_id, encode_msg(&msg, version));
	}

	/// The request of the next stream `accept_bi` would hand out, if one is ready.
	fn accepted(shared: &Arc<Shared>) -> Option<(RequestId, VirtualRecvStream)> {
		let (_, recv) = shared.incoming.pop().now_or_never()??;
		let request_id = recv.request_id.lock().unwrap().unwrap();
		Some((request_id, recv))
	}

	#[moq_clock::test]
	async fn a_readvertisement_waits_for_the_withdrawal_to_be_handled() {
		// Each request has its own handler, so a re-advertisement handed out beside the
		// withdrawal before it could be handled first and read as a second advertisement.
		for version in [Version::Draft14, Version::Draft15, Version::Draft16] {
			let shared = Arc::new(Shared::default());
			let first = receive(&shared, "live/media", version, RequestId(3)).await;
			let other = receive(&shared, "live/other", version, RequestId(5)).await;

			withdraw(&shared, "live/media", version, RequestId(3));
			readvertise(&shared, "live/media", version, RequestId(7));
			withdraw(&shared, "live/media", version, RequestId(7));
			readvertise(&shared, "live/media", version, RequestId(9));
			// Another namespace is not held up.
			withdraw(&shared, "live/other", version, RequestId(5));
			drop(other);
			readvertise(&shared, "live/other", version, RequestId(11));
			assert!(matches!(accepted(&shared), Some((RequestId(11), _))), "{version:?}");
			assert!(accepted(&shared).is_none(), "{version:?}");

			// Handling the first withdrawal hands over the next advertisement, which was
			// withdrawn too, so the one after it waits for that withdrawal in turn.
			drop(first);
			let (id, mut second) = accepted(&shared).expect("released");
			assert_eq!(id, RequestId(7), "{version:?}");
			assert!(accepted(&shared).is_none(), "{version:?}");
			assert!(drained(&mut second, 4).await, "{version:?}");
			drop(second);
			assert!(matches!(accepted(&shared), Some((RequestId(9), _))), "{version:?}");
			assert!(shared.readvertised.lock().unwrap().namespaces.is_empty(), "{version:?}");
		}
	}

	#[moq_clock::test]
	async fn test_local_withdrawal_releases_the_namespace() {
		// We withdraw our own advertisement by dropping the request that carried it, so a
		// later CANCEL must name the re-advertisement rather than the request that is gone.
		let version = Version::Draft14;
		let shared = Arc::new(Shared::default());

		let ours = advertise(&shared, "cluster/ns", version, RequestId(4)).await;
		drop(ours);
		assert!(matches!(cancel(&shared, "cluster/ns", version), Route::Ignore));

		let _again = advertise(&shared, "cluster/ns", version, RequestId(6)).await;
		assert!(matches!(
			cancel(&shared, "cluster/ns", version),
			Route::CloseStream(RequestId(6))
		));
	}

	#[moq_clock::test]
	async fn surviving_duplicate_remains_reachable_after_owner_closes() {
		for version in [Version::Draft14, Version::Draft15] {
			let shared = Arc::new(Shared::default());
			let first = receive(&shared, "cluster/ns", version, RequestId(7)).await;
			let second = receive(&shared, "cluster/ns", version, RequestId(11)).await;
			let third = receive(&shared, "cluster/ns", version, RequestId(15)).await;
			drop(first);
			assert!(matches!(
				done(&shared, "cluster/ns", version),
				Route::CloseStream(RequestId(11))
			));
			drop(second);
			assert!(matches!(
				done(&shared, "cluster/ns", version),
				Route::CloseStream(RequestId(15))
			));
			drop(third);
			assert!(matches!(done(&shared, "cluster/ns", version), Route::Ignore));
		}
	}

	#[test]
	fn namespace_removal_preserves_links_in_every_order() {
		for direction in [Direction::Incoming, Direction::Outgoing, Direction::Subscribed] {
			for first in 0..4 {
				for second in 0..4 {
					for third in 0..4 {
						if first == second || first == third || second == third {
							continue;
						}
						let fourth = (0..4).find(|id| *id != first && *id != second && *id != third).unwrap();
						let namespaces = Namespaces::default();
						let name = crate::Path::new("ns");
						for id in 0..4 {
							namespaces.register(direction, name.clone(), RequestId(id));
						}
						let mut live = [true; 4];
						for id in [first, second, third, fourth] {
							namespaces.forget(RequestId(id));
							live[id as usize] = false;
							let expected = live.iter().position(|live| *live).map(|id| RequestId(id as u64));
							assert_eq!(namespaces.get(direction, &name), expected);
						}
						let state = namespaces.state.lock().unwrap();
						assert!(state.by_request.is_empty());
						assert!(state.incoming.is_empty() && state.outgoing.is_empty() && state.subscribed.is_empty());
					}
				}
			}
		}
	}

	/// A peer's control stream carrying a bare SUBSCRIBE for each request ID: the
	/// adapter routes on the ID alone, so the rest of the body is never read.
	async fn windowed(
		window: u64,
		ids: &[u64],
		then: &[u8],
	) -> (
		ControlStreamAdapter,
		Reader<crate::transport::boxed::RecvStream, Version>,
		Writer<crate::transport::boxed::SendStream, Version>,
		crate::lite::test_transport::Log,
	) {
		use crate::lite::test_transport::{Log, ScriptedSession, SinkSend};
		use crate::transport::poll::Session as _;

		const VERSION: Version = Version::Draft14;
		let mut script = Vec::new();
		for id in ids {
			let body = make_body_with_request_id(*id, VERSION);
			script.extend_from_slice(&encode_raw(ietf::Subscribe::ID, &body, VERSION));
		}
		script.extend_from_slice(then);
		let mut session = ScriptedSession::new(script);
		let (_, recv) = session.open_bi(Default::default()).await.unwrap();
		// We are the server, so the client's request IDs are even.
		let control = Control::new(None, false).with_window(window, false, VERSION);
		let adapter = ControlStreamAdapter::new(Transport::new(session.clone()), control, VERSION);
		let log = Log::default();
		let writer = Writer::new(
			crate::transport::boxed::SendStream::new(SinkSend::new(log.clone())),
			VERSION,
		);
		(
			adapter,
			Reader::new(crate::transport::boxed::RecvStream::new(recv), VERSION),
			writer,
			log,
		)
	}

	#[moq_clock::test]
	async fn legacy_multiplexing_counts_a_control_body_once() {
		use crate::{
			coding::{Encode, Stream},
			lite::test_transport::{Log, ScriptedSession, SinkSend},
			stats::{Bytes, Control as Class, Registry, Tier},
		};
		for version in [Version::Draft14, Version::Draft15, Version::Draft16] {
			let registry = Registry::new(Default::default());
			let stats = registry.tier(Tier::default()).session("");
			let msg = ietf::FetchCancel {
				request_id: RequestId(0),
			};
			let body = msg.encode_bytes(version).unwrap();
			let raw_response = encode_raw(ietf::FetchCancel::ID, &body.slice(2..), version);
			let mut session = ScriptedSession::new(raw_response.to_vec());
			let (_, recv) = crate::transport::poll::Session::open_bi(&mut session, Default::default())
				.await
				.unwrap();
			let adapter = ControlStreamAdapter::new(Transport::new(session.clone()), Control::new(None, true), version)
				.with_stats(&stats);
			let mut virtual_stream = super::super::request_stream::with_stats(
				Stream::open(&mut crate::driver::Transport::new(adapter.clone()), version)
					.await
					.unwrap(),
				&stats,
				version,
			);
			let msg = ietf::FetchCancel {
				request_id: RequestId(0),
			};
			let size = msg.encode_bytes(version).unwrap().len() as u64;
			virtual_stream.writer.buffer_varint(ietf::FetchCancel::ID).unwrap();
			virtual_stream.writer.encode(&msg).await.unwrap();
			let raw = adapter.shared.control.pop().await.unwrap();
			let log = Log::default();
			let mut physical = Writer::new(
				crate::transport::boxed::SendStream::new(SinkSend::new(log.clone())),
				version,
			);
			physical.write_all(&mut raw.clone()).await.unwrap();
			assert_eq!(
				registry.snapshot().control(),
				vec![(Tier::default(), Class::Fetch, Bytes { tx: size, rx: 0 })],
				"{version:?}"
			);
			assert_eq!(*log.writes.lock().unwrap(), raw);
			let (_, goaway) = crate::goaway::Handle::new(false);
			let mut reading = std::pin::pin!(adapter.run_read(
				Reader::new(crate::transport::boxed::RecvStream::new(recv), version),
				goaway
			));
			assert!(futures::poll!(reading.as_mut()).is_pending());
			assert_eq!(virtual_stream.reader.varint().await.unwrap(), ietf::FetchCancel::ID);
			virtual_stream.reader.decode::<ietf::FetchCancel>().await.unwrap();
			assert_eq!(
				registry.snapshot().control(),
				vec![(Tier::default(), Class::Fetch, Bytes { tx: size, rx: size })],
				"virtual decode counted a physical frame twice: {version:?}"
			);
			adapter.send_max_request_id(RequestId(123));
			assert!(
				registry
					.snapshot()
					.control()
					.iter()
					.any(|(_, class, bytes)| *class == Class::Other && bytes.tx > 0),
				"adapter-generated messages bypassed the typed writer"
			);
		}
	}

	#[moq_clock::test]
	async fn a_legacy_control_stream_fin_or_reset_is_a_protocol_violation() {
		use crate::lite::test_transport::{DeadStreamSession, ScriptedSession};
		for version in [Version::Draft14, Version::Draft15, Version::Draft16] {
			let session = ScriptedSession::eof(Vec::new());
			let stream = crate::coding::Stream::open(&mut Transport::new(session.clone()), version)
				.await
				.unwrap();
			let adapter = ControlStreamAdapter::new(Transport::new(session.clone()), Control::new(None, true), version);
			let (_handle, protocol) = crate::goaway::Handle::new(false);
			let err = adapter.run(stream.reader, stream.writer, protocol).await.unwrap_err();
			assert!(matches!(err, Error::ProtocolViolation), "{version:?}: {err:?}");

			let session = DeadStreamSession::bis(0);
			let stream = crate::coding::Stream::open(&mut Transport::new(session.clone()), version)
				.await
				.unwrap();
			let adapter = ControlStreamAdapter::new(Transport::new(session.clone()), Control::new(None, true), version);
			let (_handle, protocol) = crate::goaway::Handle::new(false);
			let err = adapter.run(stream.reader, stream.writer, protocol).await.unwrap_err();
			assert!(matches!(err, Error::ProtocolViolation), "{version:?}: {err:?}");
		}
	}

	/// A STOP_SENDING on a legacy control stream ends the session even with nothing queued
	/// to write, while the peer keeps its own half open.
	#[moq_clock::test]
	async fn a_legacy_control_stream_stopped_while_idle_is_a_protocol_violation() {
		use crate::transport::memory;
		for version in [Version::Draft14, Version::Draft15, Version::Draft16] {
			let (session, mut peer) = memory::pair(memory::Config::default());
			let stream = crate::coding::Stream::open(&mut Transport::new(session.clone()), version)
				.await
				.unwrap();
			let mut remote = crate::coding::Stream::accept(&mut peer, version).await.unwrap();
			remote.reader.abort(&Error::Cancel);
			let adapter = ControlStreamAdapter::new(Transport::new(session.clone()), Control::new(None, true), version);
			let (_handle, protocol) = crate::goaway::Handle::new(false);
			let res = moq_clock::timeout(
				std::time::Duration::from_secs(10),
				adapter.run(stream.reader, stream.writer, protocol),
			)
			.await
			.expect("an idle writer missed the STOP_SENDING");
			assert!(matches!(res, Err(Error::ProtocolViolation)), "{version:?}: {res:?}");
		}
	}

	/// A request ID at the MAX_REQUEST_ID we advertised closes the session with the
	/// draft's TOO_MANY_REQUESTS, before the request is queued.
	#[moq_clock::test]
	async fn a_request_past_the_window_closes_the_session() {
		let (adapter, reader, writer, _) = windowed(1, &[0, 2], &[]).await;
		let (_, goaway) = crate::goaway::Handle::new(true);
		let err = adapter.run(reader, writer, goaway).await.unwrap_err();
		assert!(
			matches!(err, Error::Session(crate::SessionError::TooManyRequests)),
			"{err:?}"
		);
	}

	/// An update's own request ID counts against the window, not the subscription it targets.
	#[moq_clock::test]
	async fn an_update_past_the_window_closes_the_session() {
		const VERSION: Version = Version::Draft14;
		let update = ietf::SubscribeUpdate {
			max_age: None,
			new_group: None,
			request_id: RequestId(4),
			subscription_request_id: Some(RequestId(0)),
			start_location: ietf::Location { group: 0, object: 0 },
			end_group: 0,
			subscriber_priority: Some(1),
			forward: Some(true),
			filter: Some(ietf::Filter::NextObject),
			authorization_token: None,
			fill: None,
		};
		let update = encode_raw(ietf::SubscribeUpdate::ID, &encode_body(&update, VERSION), VERSION);
		// The target is in the window and the update's own ID is past it.
		let (adapter, reader, writer, _) = windowed(2, &[0], &update).await;
		let (_, goaway) = crate::goaway::Handle::new(true);
		let err = adapter.run(reader, writer, goaway).await.unwrap_err();
		assert!(
			matches!(err, Error::Session(crate::SessionError::TooManyRequests)),
			"{err:?}"
		);
	}

	/// An update aimed at no open request, such as draft 16's SUBSCRIBE_NAMESPACE riding a
	/// stream of its own, still gets its one answer on drafts 15 and 16: a REQUEST_ERROR
	/// naming the update. Draft 14 answers no update.
	#[moq_clock::test]
	async fn an_unroutable_update_is_refused() {
		use crate::lite::test_transport::{Log, ScriptedSession, SinkSend};
		use crate::transport::poll::Session as _;

		for version in [Version::Draft14, Version::Draft15, Version::Draft16] {
			// Own Request ID 2, aimed at request 0, which nothing here holds. Draft 14 carries
			// its fixed start, end group, priority and forward before the parameters.
			let body: &[u8] = match version {
				Version::Draft14 => &[2, 0, 0, 0, 0, 128, 1, 0],
				_ => &[2, 0, 0],
			};
			let script = encode_raw(ietf::SubscribeUpdate::ID, &Bytes::copy_from_slice(body), version);
			let mut session = ScriptedSession::new(script.to_vec());
			let (_, recv) = session.open_bi(Default::default()).await.unwrap();
			let adapter =
				ControlStreamAdapter::new(Transport::new(session.clone()), Control::new(None, false), version);
			let log = Log::default();
			let writer = Writer::new(
				crate::transport::boxed::SendStream::new(SinkSend::new(log.clone())),
				version,
			);
			let (_, goaway) = crate::goaway::Handle::new(true);
			let mut run = std::pin::pin!(adapter.run(
				Reader::new(crate::transport::boxed::RecvStream::new(recv), version),
				writer,
				goaway
			));
			assert!(futures::poll!(run.as_mut()).is_pending(), "{version}");

			let written = log.writes.lock().unwrap().clone();
			match version {
				Version::Draft14 => assert!(written.is_empty(), "{version}: {written:?}"),
				// [type, length (2), Request ID, NOT_SUPPORTED, ...]
				_ => assert_eq!(
					(written[0], written[3], written[4]),
					(ietf::RequestError::ID as u8, 2, 0x3),
					"{version}: {written:?}"
				),
			}
		}
	}

	/// Requests that end are granted back with MAX_REQUEST_ID, once half the window has.
	#[moq_clock::test]
	async fn retired_requests_are_granted_back() {
		use crate::transport::poll::Session as _;

		const VERSION: Version = Version::Draft14;
		let (mut adapter, reader, writer, log) = windowed(2, &[0, 2], &[]).await;
		let (_, goaway) = crate::goaway::Handle::new(true);
		let runner = adapter.clone();
		let mut run = std::pin::pin!(runner.run(reader, writer, goaway));
		assert!(futures::poll!(run.as_mut()).is_pending());

		let first = adapter.accept_bi().await.unwrap();
		let _second = adapter.accept_bi().await.unwrap();
		assert!(log.writes.lock().unwrap().is_empty(), "nothing has ended yet");

		drop(first);
		assert!(futures::poll!(run.as_mut()).is_pending());

		let mut expected = Vec::new();
		crate::coding::Encoder::new(&mut expected, VERSION.into())
			.varint(ietf::MaxRequestId::ID)
			.unwrap();
		ietf::MaxRequestId {
			request_id: RequestId(6),
		}
		.encode(&mut crate::coding::Encoder::new(&mut expected, VERSION.into()), VERSION)
		.unwrap();
		assert_eq!(*log.writes.lock().unwrap(), expected.to_vec());
	}

	#[test]
	fn queued_incoming_stream_does_not_keep_shared_alive() {
		let shared = Arc::new(Shared::default());
		let weak = Arc::downgrade(&shared);
		shared.open_incoming(RequestId(7), Bytes::new(), None).unwrap();
		drop(shared);
		assert!(weak.upgrade().is_none());
	}

	#[moq_clock::test]
	async fn receive_drop_before_registration_releases_namespace() {
		for version in [Version::Draft14, Version::Draft15] {
			let shared = Arc::new(Shared::default());
			let (mut send, recv) = shared.open_outgoing(version);
			drop(recv);
			send.write_chunk(encode_msg(&publish_namespace(RequestId(4), "cluster/ns"), version))
				.await
				.unwrap();
			assert!(shared.streams.lock().unwrap().is_empty());
			assert!(matches!(cancel(&shared, "cluster/ns", version), Route::Ignore));
			let _again = advertise(&shared, "cluster/ns", version, RequestId(6)).await;
			assert!(matches!(
				cancel(&shared, "cluster/ns", version),
				Route::CloseStream(RequestId(6))
			));
		}
	}

	#[test]
	fn test_unresolvable_withdrawal_is_dropped() {
		// v14/v15 name their withdrawals, and a name we hold no advertisement for belongs
		// to a request that is already gone. Dropping it beats ending the session.
		for version in [Version::Draft14, Version::Draft15] {
			let shared = Arc::new(Shared::default());
			assert!(matches!(done(&shared, "cluster/ns", version), Route::Ignore));
			assert!(matches!(cancel(&shared, "cluster/ns", version), Route::Ignore));
		}
	}
}
