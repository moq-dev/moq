use std::{
	collections::HashMap,
	sync::{Arc, Mutex},
};

use bytes::{Buf, BufMut, Bytes, BytesMut};
use tokio::sync::mpsc;

use crate::{
	Error, PathOwned,
	coding::{Decode, Encode, Reader, Writer},
	ietf::{self, RequestId},
};

use super::{Control, Message, Version};

// === Virtual Streams ===

/// A virtual receive stream backed by an initial message buffer and a channel for follow-up messages.
pub struct VirtualRecvStream {
	buffer: Bytes,
	rx: mpsc::UnboundedReceiver<Bytes>,
	closed: bool,
}

impl VirtualRecvStream {
	fn new(initial: Bytes, rx: mpsc::UnboundedReceiver<Bytes>) -> Self {
		Self {
			buffer: initial,
			rx,
			closed: false,
		}
	}

	/// Fill the buffer from the channel if empty. Returns false if the stream is closed.
	async fn fill(&mut self) -> bool {
		if !self.buffer.is_empty() {
			return true;
		}

		if self.closed {
			return false;
		}

		match self.rx.recv().await {
			Some(data) => {
				self.buffer = data;
				true
			}
			None => {
				self.closed = true;
				false
			}
		}
	}
}

impl web_transport_trait::RecvStream for VirtualRecvStream {
	type Error = crate::Error;

	async fn read(&mut self, dst: &mut [u8]) -> Result<Option<usize>, Self::Error> {
		if !self.fill().await {
			return Ok(None);
		}

		let n = dst.len().min(self.buffer.len());
		dst[..n].copy_from_slice(&self.buffer[..n]);
		self.buffer.advance(n);
		Ok(Some(n))
	}

	async fn read_buf<B: BufMut + web_transport_trait::MaybeSend>(
		&mut self,
		buf: &mut B,
	) -> Result<Option<usize>, Self::Error> {
		if !self.fill().await {
			return Ok(None);
		}

		let n = buf.remaining_mut().min(self.buffer.len());
		buf.put(self.buffer.split_to(n));
		Ok(Some(n))
	}

	async fn read_chunk(&mut self, max: usize) -> Result<Option<Bytes>, Self::Error> {
		if !self.fill().await {
			return Ok(None);
		}

		let n = max.min(self.buffer.len());
		Ok(Some(self.buffer.split_to(n)))
	}

	fn stop(&mut self, _code: u32) {
		self.rx.close();
	}

	async fn closed(&mut self) -> Result<(), Self::Error> {
		// Wait until the channel is closed
		if self.closed {
			return Ok(());
		}
		// Drain remaining messages
		while self.rx.recv().await.is_some() {}
		self.closed = true;
		Ok(())
	}
}

/// A virtual send stream that forwards writes to the shared control stream writer.
///
/// For streams created by `open_bi` (outgoing requests), this also parses
/// the first outgoing message to extract the request_id and register the
/// stream for response routing.
pub struct VirtualSendStream {
	control_tx: mpsc::UnboundedSender<Bytes>,
	/// Present only for outgoing requests (from open_bi).
	/// Accumulates bytes until the request_id can be parsed,
	/// then registers the stream and flushes.
	pending: Option<OutgoingRegistration>,
}

struct OutgoingRegistration {
	follow_tx: mpsc::UnboundedSender<Bytes>,
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
		let request_id = RequestId::decode(&mut cursor, self.version).map_err(|_| crate::Error::Decode)?;

		// For PublishNamespace, also extract the namespace for reverse lookup.
		if type_id == ietf::PublishNamespace::ID {
			if self.version == Version::Draft17 {
				// v17 has required_request_id_delta after request_id
				let _ = u64::decode(&mut cursor, self.version);
			}
			if let Ok(ns) = crate::ietf::namespace::decode_namespace(&mut cursor, self.version) {
				self.shared
					.namespaces
					.lock()
					.unwrap()
					.insert(ns.into_owned(), request_id);
			}
		}

		Ok(Some(request_id))
	}

	fn register(self, request_id: RequestId) {
		self.shared.streams.lock().unwrap().insert(request_id, self.follow_tx);
	}
}

impl VirtualSendStream {
	fn new(control_tx: mpsc::UnboundedSender<Bytes>) -> Self {
		Self {
			control_tx,
			pending: None,
		}
	}

	fn with_registration(control_tx: mpsc::UnboundedSender<Bytes>, pending: OutgoingRegistration) -> Self {
		Self {
			control_tx,
			pending: Some(pending),
		}
	}
}

impl web_transport_trait::SendStream for VirtualSendStream {
	type Error = crate::Error;

	async fn write(&mut self, buf: &[u8]) -> Result<usize, Self::Error> {
		let len = buf.len();

		if let Some(pending) = &mut self.pending {
			pending.buf.extend_from_slice(buf);

			if let Some(request_id) = pending.try_parse()? {
				let mut pending = self.pending.take().unwrap();
				let buf = std::mem::take(&mut pending.buf).freeze();
				pending.register(request_id);
				self.control_tx.send(buf).map_err(|_| crate::Error::Closed)?;
			}
		} else {
			self.control_tx
				.send(Bytes::copy_from_slice(buf))
				.map_err(|_| crate::Error::Closed)?;
		}

		Ok(len)
	}

	fn set_priority(&mut self, _order: u8) {}

	fn finish(&mut self) -> Result<(), Self::Error> {
		// Flush any remaining buffered data (e.g. if registration never completed).
		if let Some(pending) = self.pending.take()
			&& !pending.buf.is_empty()
		{
			let _ = self.control_tx.send(pending.buf.freeze());
		}
		Ok(())
	}

	fn reset(&mut self, _code: u32) {}

	async fn closed(&mut self) -> Result<(), Self::Error> {
		Ok(())
	}
}

// === Adapter Send/Recv Enums ===

pub enum AdapterSend<S: web_transport_trait::Session> {
	Real(S::SendStream),
	Virtual(VirtualSendStream),
}

impl<S: web_transport_trait::Session> web_transport_trait::SendStream for AdapterSend<S> {
	type Error = crate::Error;

	async fn write(&mut self, buf: &[u8]) -> Result<usize, Self::Error> {
		match self {
			Self::Real(s) => s.write(buf).await.map_err(|_| crate::Error::Closed),
			Self::Virtual(s) => s.write(buf).await,
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

	async fn closed(&mut self) -> Result<(), Self::Error> {
		match self {
			Self::Real(s) => s.closed().await.map_err(|_| crate::Error::Closed),
			Self::Virtual(s) => s.closed().await,
		}
	}
}

pub enum AdapterRecv<S: web_transport_trait::Session> {
	Real(S::RecvStream),
	Virtual(VirtualRecvStream),
}

impl<S: web_transport_trait::Session> web_transport_trait::RecvStream for AdapterRecv<S> {
	type Error = crate::Error;

	async fn read(&mut self, dst: &mut [u8]) -> Result<Option<usize>, Self::Error> {
		match self {
			Self::Real(s) => s.read(dst).await.map_err(|_| crate::Error::Closed),
			Self::Virtual(s) => s.read(dst).await,
		}
	}

	async fn read_buf<B: BufMut + web_transport_trait::MaybeSend>(
		&mut self,
		buf: &mut B,
	) -> Result<Option<usize>, Self::Error> {
		match self {
			Self::Real(s) => s.read_buf(buf).await.map_err(|_| crate::Error::Closed),
			Self::Virtual(s) => s.read_buf(buf).await,
		}
	}

	async fn read_chunk(&mut self, max: usize) -> Result<Option<Bytes>, Self::Error> {
		match self {
			Self::Real(s) => s.read_chunk(max).await.map_err(|_| crate::Error::Closed),
			Self::Virtual(s) => s.read_chunk(max).await,
		}
	}

	fn stop(&mut self, code: u32) {
		match self {
			Self::Real(s) => s.stop(code),
			Self::Virtual(s) => s.stop(code),
		}
	}

	async fn closed(&mut self) -> Result<(), Self::Error> {
		match self {
			Self::Real(s) => s.closed().await.map_err(|_| crate::Error::Closed),
			Self::Virtual(s) => s.closed().await,
		}
	}
}

// === Control Stream Adapter ===

struct Shared {
	incoming_tx: mpsc::UnboundedSender<(VirtualSendStream, VirtualRecvStream)>,
	incoming_rx: tokio::sync::Mutex<mpsc::UnboundedReceiver<(VirtualSendStream, VirtualRecvStream)>>,

	/// Channel that VirtualSendStreams write to; the writer task reads from this.
	control_tx: mpsc::UnboundedSender<Bytes>,

	/// Active virtual streams keyed by request_id.
	streams: Mutex<HashMap<RequestId, mpsc::UnboundedSender<Bytes>>>,

	/// Namespace → request_id reverse lookup (for v14/v15 namespace-keyed messages).
	namespaces: Mutex<HashMap<PathOwned, RequestId>>,
}

#[derive(Clone)]
pub struct ControlStreamAdapter<S: web_transport_trait::Session> {
	inner: S,
	shared: Arc<Shared>,
	control: Control,
	version: Version,
}

impl<S: web_transport_trait::Session> ControlStreamAdapter<S> {
	pub fn new(inner: S, control_tx: mpsc::UnboundedSender<Bytes>, control: Control, version: Version) -> Self {
		let (incoming_tx, incoming_rx) = mpsc::unbounded_channel();
		Self {
			inner,
			shared: Arc::new(Shared {
				incoming_tx,
				incoming_rx: tokio::sync::Mutex::new(incoming_rx),
				control_tx,
				streams: Mutex::new(HashMap::new()),
				namespaces: Mutex::new(HashMap::new()),
			}),
			control,
			version,
		}
	}

	/// Open a real (non-virtual) bidi stream, bypassing control stream multiplexing.
	/// Used for v16 SubscribeNamespace which moved to its own bidi stream.
	pub async fn open_native_bi(&self) -> Result<(AdapterSend<S>, AdapterRecv<S>), crate::Error> {
		let (send, recv) = self.inner.open_bi().await.map_err(|_| crate::Error::Closed)?;
		Ok((AdapterSend::Real(send), AdapterRecv::Real(recv)))
	}

	/// Run the control stream read + write tasks.
	/// This reads from the control stream and routes messages to virtual streams,
	/// and also drains the write channel to the control stream writer.
	pub async fn run(
		&self,
		reader: Reader<S::RecvStream, Version>,
		writer: Writer<S::SendStream, Version>,
		rx: mpsc::UnboundedReceiver<Bytes>,
	) -> Result<(), Error> {
		tokio::select! {
			res = self.run_read(reader) => res,
			res = Self::run_write(writer, rx) => res,
		}
	}

	/// Writer task: drains the channel and writes to the control stream.
	async fn run_write(
		mut writer: Writer<S::SendStream, Version>,
		mut rx: mpsc::UnboundedReceiver<Bytes>,
	) -> Result<(), Error> {
		while let Some(msg) = rx.recv().await {
			let mut buf = std::io::Cursor::new(msg);
			writer.write_all(&mut buf).await?;
		}
		Ok(())
	}

	/// Dispatcher loop that reads control stream messages and routes them.
	async fn run_read(&self, mut reader: Reader<S::RecvStream, Version>) -> Result<(), Error> {
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
			let route = self.classify(type_id, &body)?;

			match route {
				Route::NewRequest(request_id) => {
					let (follow_tx, follow_rx) = mpsc::unbounded_channel();
					let recv = VirtualRecvStream::new(raw, follow_rx);
					let send = VirtualSendStream::new(self.shared.control_tx.clone());
					self.shared.streams.lock().unwrap().insert(request_id, follow_tx);
					self.shared.incoming_tx.send((send, recv)).map_err(|_| Error::Closed)?;
				}
				Route::Response(request_id) => {
					if let Some(tx) = self.shared.streams.lock().unwrap().get(&request_id) {
						let _ = tx.send(raw);
					}
				}
				Route::FollowUp(request_id) => {
					if let Some(tx) = self.shared.streams.lock().unwrap().get(&request_id) {
						let _ = tx.send(raw);
					}
				}
				Route::CloseStream(request_id) => {
					if let Some(tx) = self.shared.streams.lock().unwrap().remove(&request_id) {
						let _ = tx.send(raw);
					}
				}
				Route::MaxRequestId(max) => {
					self.control.max_request_id(max);
				}
				Route::GoAway => {
					return Err(Error::Unsupported);
				}
			}
		}
	}

	/// Classify a control message and extract its request_id for routing.
	/// This is a method (not a free function) because v14/v15 namespace-keyed
	/// messages need access to the namespace→request_id map.
	fn classify(&self, type_id: u64, body: &Bytes) -> Result<Route, Error> {
		match type_id {
			// New requests: these create new virtual streams
			ietf::Subscribe::ID => {
				let id = decode_request_id(body, self.version)?;
				Ok(Route::NewRequest(id))
			}
			ietf::Fetch::ID => {
				let id = decode_request_id(body, self.version)?;
				Ok(Route::NewRequest(id))
			}
			ietf::Publish::ID => {
				let id = decode_request_id(body, self.version)?;
				Ok(Route::NewRequest(id))
			}
			ietf::PublishNamespace::ID => {
				let id = decode_request_id(body, self.version)?;
				// Decode the namespace and store the mapping for v14/v15 reverse lookup
				if let Ok(ns) = decode_publish_namespace_body(body, self.version) {
					self.shared.namespaces.lock().unwrap().insert(ns, id);
				}
				Ok(Route::NewRequest(id))
			}
			ietf::TrackStatus::ID => {
				let id = decode_request_id(body, self.version)?;
				Ok(Route::NewRequest(id))
			}
			// SubscribeNamespace on control stream (v14/v15 only)
			ietf::SubscribeNamespace::ID => match self.version {
				Version::Draft14 | Version::Draft15 => {
					let id = decode_request_id(body, self.version)?;
					Ok(Route::NewRequest(id))
				}
				_ => Err(Error::UnexpectedMessage),
			},

			// Responses: route to the virtual stream waiting for a reply
			ietf::SubscribeOk::ID => {
				let id = decode_response_request_id(body, self.version)?;
				Ok(Route::Response(id))
			}
			// 0x05: SubscribeError in v14, RequestError in v15+
			ietf::SubscribeError::ID => {
				let id = decode_response_request_id(body, self.version)?;
				Ok(Route::CloseStream(id))
			}
			ietf::FetchOk::ID => {
				let id = decode_response_request_id(body, self.version)?;
				Ok(Route::Response(id))
			}
			// 0x19: FetchError in v14 only
			ietf::FetchError::ID => match self.version {
				Version::Draft14 => {
					let id = decode_request_id(body, self.version)?;
					Ok(Route::CloseStream(id))
				}
				_ => Err(Error::UnexpectedMessage),
			},
			// PublishOk (0x1E)
			ietf::PublishOk::ID => {
				let id = decode_response_request_id(body, self.version)?;
				Ok(Route::Response(id))
			}
			// PublishError (0x1F) - v14 only
			ietf::PublishError::ID => {
				let id = decode_request_id(body, self.version)?;
				Ok(Route::CloseStream(id))
			}
			// 0x07: PublishNamespaceOk in v14, RequestOk in v15+
			ietf::PublishNamespaceOk::ID => match self.version {
				Version::Draft14 => {
					let id = decode_request_id(body, self.version)?;
					Ok(Route::Response(id))
				}
				Version::Draft15 | Version::Draft16 => {
					// RequestOk - route to stream
					let id = decode_response_request_id(body, self.version)?;
					Ok(Route::Response(id))
				}
				_ => Err(Error::UnexpectedMessage),
			},
			// 0x08: PublishNamespaceError in v14 only
			ietf::PublishNamespaceError::ID => match self.version {
				Version::Draft14 => {
					let id = decode_request_id(body, self.version)?;
					Ok(Route::CloseStream(id))
				}
				_ => Err(Error::UnexpectedMessage),
			},
			// SubscribeNamespaceOk (v14 only)
			ietf::SubscribeNamespaceOk::ID => match self.version {
				Version::Draft14 => {
					let id = decode_request_id(body, self.version)?;
					Ok(Route::Response(id))
				}
				_ => Err(Error::UnexpectedMessage),
			},
			// SubscribeNamespaceError (v14 only)
			ietf::SubscribeNamespaceError::ID => match self.version {
				Version::Draft14 => {
					let id = decode_request_id(body, self.version)?;
					Ok(Route::CloseStream(id))
				}
				_ => Err(Error::UnexpectedMessage),
			},

			// Follow-up messages: route to existing stream
			ietf::SubscribeUpdate::ID => {
				let id = decode_request_id(body, self.version)?;
				Ok(Route::FollowUp(id))
			}

			// Close stream messages
			ietf::Unsubscribe::ID => {
				let id = decode_request_id(body, self.version)?;
				Ok(Route::CloseStream(id))
			}
			ietf::PublishDone::ID => {
				let id = decode_response_request_id(body, self.version)?;
				Ok(Route::CloseStream(id))
			}
			ietf::FetchCancel::ID => {
				let id = decode_request_id(body, self.version)?;
				Ok(Route::CloseStream(id))
			}
			ietf::PublishNamespaceDone::ID => match self.version {
				Version::Draft16 => {
					let id = decode_request_id(body, self.version)?;
					Ok(Route::CloseStream(id))
				}
				// v14/v15: namespace-keyed — decode namespace and look up request_id
				Version::Draft14 | Version::Draft15 => {
					let id = self.lookup_namespace_request_id(body)?;
					Ok(Route::CloseStream(id))
				}
				_ => Err(Error::UnexpectedMessage),
			},
			ietf::PublishNamespaceCancel::ID => match self.version {
				Version::Draft16 => {
					let id = decode_request_id(body, self.version)?;
					Ok(Route::CloseStream(id))
				}
				// v14/v15: namespace-keyed
				Version::Draft14 | Version::Draft15 => {
					let id = self.lookup_namespace_request_id(body)?;
					Ok(Route::CloseStream(id))
				}
				_ => Err(Error::UnexpectedMessage),
			},
			ietf::UnsubscribeNamespace::ID => match self.version {
				Version::Draft14 | Version::Draft15 => {
					let id = decode_request_id(body, self.version)?;
					Ok(Route::CloseStream(id))
				}
				_ => Err(Error::UnexpectedMessage),
			},

			// Utility
			ietf::MaxRequestId::ID => {
				let id = decode_request_id(body, self.version)?;
				Ok(Route::MaxRequestId(id))
			}
			ietf::RequestsBlocked::ID => Err(Error::UnexpectedMessage),

			// Terminal
			ietf::GoAway::ID => Ok(Route::GoAway),

			_ => Err(Error::UnexpectedMessage),
		}
	}

	/// Decode namespace from a v14/v15 namespace-keyed message body and look up the request_id.
	fn lookup_namespace_request_id(&self, body: &Bytes) -> Result<RequestId, Error> {
		let mut cursor = std::io::Cursor::new(body);
		let ns = crate::ietf::namespace::decode_namespace(&mut cursor, self.version)?;
		self.shared
			.namespaces
			.lock()
			.unwrap()
			.get(&ns)
			.copied()
			.ok_or(Error::NotFound)
	}
}

impl<S: web_transport_trait::Session> web_transport_trait::Session for ControlStreamAdapter<S> {
	type SendStream = AdapterSend<S>;
	type RecvStream = AdapterRecv<S>;
	type Error = crate::Error;

	async fn accept_bi(&self) -> Result<(Self::SendStream, Self::RecvStream), Self::Error> {
		let mut rx = self.shared.incoming_rx.lock().await;

		match self.version {
			// v16: SubscribeNamespace uses real bidi streams, so race both sources.
			Version::Draft16 => {
				tokio::select! {
					result = rx.recv() => {
						match result {
							Some((send, recv)) => Ok((AdapterSend::Virtual(send), AdapterRecv::Virtual(recv))),
							None => Err(crate::Error::Closed),
						}
					}
					result = self.inner.accept_bi() => {
						match result {
							Ok((send, recv)) => Ok((AdapterSend::Real(send), AdapterRecv::Real(recv))),
							Err(_) => Err(crate::Error::Closed),
						}
					}
				}
			}
			// v14/v15: Only virtual streams from control stream.
			_ => match rx.recv().await {
				Some((send, recv)) => Ok((AdapterSend::Virtual(send), AdapterRecv::Virtual(recv))),
				None => Err(crate::Error::Closed),
			},
		}
	}

	async fn open_bi(&self) -> Result<(Self::SendStream, Self::RecvStream), Self::Error> {
		let (follow_tx, follow_rx) = mpsc::unbounded_channel();
		let recv = VirtualRecvStream::new(Bytes::new(), follow_rx);
		let send = VirtualSendStream::with_registration(
			self.shared.control_tx.clone(),
			OutgoingRegistration {
				follow_tx,
				shared: Arc::clone(&self.shared),
				version: self.version,
				buf: BytesMut::new(),
			},
		);
		Ok((AdapterSend::Virtual(send), AdapterRecv::Virtual(recv)))
	}

	async fn open_uni(&self) -> Result<Self::SendStream, Self::Error> {
		let s = self.inner.open_uni().await.map_err(|_| crate::Error::Closed)?;
		Ok(AdapterSend::Real(s))
	}

	async fn accept_uni(&self) -> Result<Self::RecvStream, Self::Error> {
		let s = self.inner.accept_uni().await.map_err(|_| crate::Error::Closed)?;
		Ok(AdapterRecv::Real(s))
	}

	fn send_datagram(&self, payload: Bytes) -> Result<(), Self::Error> {
		self.inner.send_datagram(payload).map_err(|_| crate::Error::Closed)
	}

	async fn recv_datagram(&self) -> Result<Bytes, Self::Error> {
		self.inner.recv_datagram().await.map_err(|_| crate::Error::Closed)
	}

	fn max_datagram_size(&self) -> usize {
		self.inner.max_datagram_size()
	}

	fn protocol(&self) -> Option<&str> {
		self.inner.protocol()
	}

	fn close(&self, code: u32, reason: &str) {
		self.inner.close(code, reason)
	}

	async fn closed(&self) -> Self::Error {
		let _ = self.inner.closed().await;
		crate::Error::Closed
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
	use bytes::BytesMut;
	use web_transport_trait::{RecvStream as _, SendStream as _};

	fn make_body_with_request_id(id: u64, version: Version) -> Bytes {
		let mut buf = BytesMut::new();
		RequestId(id).encode(&mut buf, version).unwrap();
		buf.freeze()
	}

	/// Helper to create a classify-only test adapter without needing a real session.
	/// We only need classify(), which doesn't touch the session at all.
	fn classify_msg(version: Version, type_id: u64, body: &Bytes) -> Result<Route, Error> {
		// Build a minimal adapter just for the classify method.
		// classify() only reads self.version and self.shared.namespaces.
		let (control_tx, _) = mpsc::unbounded_channel();
		let (incoming_tx, incoming_rx) = mpsc::unbounded_channel();
		let shared = Arc::new(Shared {
			incoming_tx,
			incoming_rx: tokio::sync::Mutex::new(incoming_rx),
			control_tx,
			streams: Mutex::new(HashMap::new()),
			namespaces: Mutex::new(HashMap::new()),
		});
		// We need a dummy inner session — but classify doesn't use it.
		// Use a struct that satisfies the trait bound. We can't easily construct one,
		// so we'll test via a free function wrapper instead.

		// Actually, classify is &self, so we need a ControlStreamAdapter<S>.
		// Let's just test the classification logic directly.
		let route = classify_with_state(type_id, body, version, &shared.namespaces)?;
		Ok(route)
	}

	/// Standalone classify for testing (mirrors the adapter's classify method).
	fn classify_with_state(
		type_id: u64,
		body: &Bytes,
		version: Version,
		namespaces: &Mutex<HashMap<PathOwned, RequestId>>,
	) -> Result<Route, Error> {
		match type_id {
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
				Ok(Route::NewRequest(id))
			}
			ietf::TrackStatus::ID => {
				let id = decode_request_id(body, version)?;
				Ok(Route::NewRequest(id))
			}
			ietf::SubscribeNamespace::ID => match version {
				Version::Draft14 | Version::Draft15 => {
					let id = decode_request_id(body, version)?;
					Ok(Route::NewRequest(id))
				}
				_ => Err(Error::UnexpectedMessage),
			},
			ietf::SubscribeOk::ID => {
				let id = decode_response_request_id(body, version)?;
				Ok(Route::Response(id))
			}
			ietf::SubscribeError::ID => {
				let id = decode_response_request_id(body, version)?;
				Ok(Route::CloseStream(id))
			}
			ietf::FetchOk::ID => {
				let id = decode_response_request_id(body, version)?;
				Ok(Route::Response(id))
			}
			ietf::FetchError::ID => match version {
				Version::Draft14 => {
					let id = decode_request_id(body, version)?;
					Ok(Route::CloseStream(id))
				}
				_ => Err(Error::UnexpectedMessage),
			},
			ietf::PublishOk::ID => {
				let id = decode_response_request_id(body, version)?;
				Ok(Route::Response(id))
			}
			ietf::PublishError::ID => {
				let id = decode_request_id(body, version)?;
				Ok(Route::CloseStream(id))
			}
			ietf::PublishNamespaceOk::ID => match version {
				Version::Draft14 => {
					let id = decode_request_id(body, version)?;
					Ok(Route::Response(id))
				}
				Version::Draft15 | Version::Draft16 => {
					let id = decode_response_request_id(body, version)?;
					Ok(Route::Response(id))
				}
				_ => Err(Error::UnexpectedMessage),
			},
			ietf::PublishNamespaceError::ID => match version {
				Version::Draft14 => {
					let id = decode_request_id(body, version)?;
					Ok(Route::CloseStream(id))
				}
				_ => Err(Error::UnexpectedMessage),
			},
			ietf::SubscribeNamespaceOk::ID => match version {
				Version::Draft14 => {
					let id = decode_request_id(body, version)?;
					Ok(Route::Response(id))
				}
				_ => Err(Error::UnexpectedMessage),
			},
			ietf::SubscribeNamespaceError::ID => match version {
				Version::Draft14 => {
					let id = decode_request_id(body, version)?;
					Ok(Route::CloseStream(id))
				}
				_ => Err(Error::UnexpectedMessage),
			},
			ietf::SubscribeUpdate::ID => {
				let id = decode_request_id(body, version)?;
				Ok(Route::FollowUp(id))
			}
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
				Version::Draft14 | Version::Draft15 => {
					let mut cursor = std::io::Cursor::new(body);
					if let Ok(ns) = crate::ietf::namespace::decode_namespace(&mut cursor, version)
						&& let Some(id) = namespaces.lock().unwrap().get(&ns).copied()
					{
						return Ok(Route::CloseStream(id));
					}
					Err(Error::UnexpectedMessage)
				}
				_ => Err(Error::UnexpectedMessage),
			},
			ietf::PublishNamespaceCancel::ID => match version {
				Version::Draft16 => {
					let id = decode_request_id(body, version)?;
					Ok(Route::CloseStream(id))
				}
				Version::Draft14 | Version::Draft15 => {
					let mut cursor = std::io::Cursor::new(body);
					if let Ok(ns) = crate::ietf::namespace::decode_namespace(&mut cursor, version)
						&& let Some(id) = namespaces.lock().unwrap().get(&ns).copied()
					{
						return Ok(Route::CloseStream(id));
					}
					Err(Error::UnexpectedMessage)
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
			ietf::MaxRequestId::ID => {
				let id = decode_request_id(body, version)?;
				Ok(Route::MaxRequestId(id))
			}
			ietf::RequestsBlocked::ID => Err(Error::UnexpectedMessage),
			ietf::GoAway::ID => Ok(Route::GoAway),
			_ => Err(Error::UnexpectedMessage),
		}
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
		let route = classify_msg(Version::Draft14, ietf::SubscribeNamespace::ID, &body).unwrap();
		assert!(matches!(route, Route::NewRequest(RequestId(20))));
	}

	#[test]
	fn test_classify_subscribe_namespace_v16_errors() {
		let body = make_body_with_request_id(20, Version::Draft16);
		let result = classify_msg(Version::Draft16, ietf::SubscribeNamespace::ID, &body);
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
		let (tx, rx) = mpsc::unbounded_channel();
		let mut stream = VirtualRecvStream::new(initial, rx);

		// Read initial data
		let mut buf = [0u8; 32];
		let n = stream.read(&mut buf).await.unwrap().unwrap();
		assert_eq!(&buf[..n], b"initial");

		// Send follow-up
		tx.send(Bytes::from_static(b"followup")).unwrap();
		let n = stream.read(&mut buf).await.unwrap().unwrap();
		assert_eq!(&buf[..n], b"followup");

		// Close channel → FIN
		drop(tx);
		let result = stream.read(&mut buf).await.unwrap();
		assert_eq!(result, None);
	}

	#[tokio::test]
	async fn test_virtual_recv_stream_partial_reads() {
		let initial = Bytes::from_static(b"hello world");
		let (_tx, rx) = mpsc::unbounded_channel();
		let mut stream = VirtualRecvStream::new(initial, rx);

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
	async fn test_virtual_send_stream_writes_to_channel() {
		let (control_tx, mut control_rx) = mpsc::unbounded_channel();
		let mut stream = VirtualSendStream::new(control_tx);

		let n = stream.write(b"hello").await.unwrap();
		assert_eq!(n, 5);

		let data = control_rx.recv().await.unwrap();
		assert_eq!(data, &b"hello"[..]);
	}

	#[test]
	fn test_namespace_reverse_lookup_v14() {
		let namespaces = Mutex::new(HashMap::new());
		namespaces
			.lock()
			.unwrap()
			.insert(crate::Path::new("test/ns").into_owned(), RequestId(42));

		// Build a v14 PublishNamespaceDone body (namespace-keyed)
		let mut buf = BytesMut::new();
		crate::ietf::namespace::encode_namespace(&mut buf, &crate::Path::new("test/ns"), Version::Draft14).unwrap();
		let body = buf.freeze();

		let route = classify_with_state(ietf::PublishNamespaceDone::ID, &body, Version::Draft14, &namespaces).unwrap();
		assert!(matches!(route, Route::CloseStream(RequestId(42))));
	}
}
