use std::{
	collections::HashMap,
	sync::{Arc, Mutex},
};

use bytes::{Bytes, BytesMut};
use tokio::sync::mpsc;

use crate::{
	Error,
	coding::{Decode, Encode, Reader},
	ietf::{self, RequestId},
};

use super::{Message, Version};

// === Virtual Streams ===

/// A virtual receive stream backed by an initial message buffer and a channel for follow-up messages.
pub struct VirtualRecvStream {
	buffer: BytesMut,
	rx: mpsc::UnboundedReceiver<Bytes>,
	closed: bool,
}

impl VirtualRecvStream {
	fn new(initial: Bytes, rx: mpsc::UnboundedReceiver<Bytes>) -> Self {
		Self {
			buffer: BytesMut::from(initial.as_ref()),
			rx,
			closed: false,
		}
	}
}

impl web_transport_trait::RecvStream for VirtualRecvStream {
	type Error = AdapterError;

	async fn read(&mut self, dst: &mut [u8]) -> Result<Option<usize>, Self::Error> {
		loop {
			// Drain buffer first
			if !self.buffer.is_empty() {
				let n = dst.len().min(self.buffer.len());
				dst[..n].copy_from_slice(&self.buffer[..n]);
				self.buffer = self.buffer.split_off(n);
				return Ok(Some(n));
			}

			if self.closed {
				return Ok(None);
			}

			// Wait for more data from channel
			match self.rx.recv().await {
				Some(data) => {
					self.buffer = BytesMut::from(data.as_ref());
				}
				None => {
					self.closed = true;
					return Ok(None);
				}
			}
		}
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
pub struct VirtualSendStream {
	control_tx: mpsc::UnboundedSender<Vec<u8>>,
}

impl VirtualSendStream {
	fn new(control_tx: mpsc::UnboundedSender<Vec<u8>>) -> Self {
		Self { control_tx }
	}
}

impl web_transport_trait::SendStream for VirtualSendStream {
	type Error = AdapterError;

	async fn write(&mut self, buf: &[u8]) -> Result<usize, Self::Error> {
		self.control_tx.send(buf.to_vec()).map_err(|_| AdapterError::Closed)?;
		Ok(buf.len())
	}

	fn set_priority(&mut self, _order: u8) {}

	fn finish(&mut self) -> Result<(), Self::Error> {
		Ok(())
	}

	fn reset(&mut self, _code: u32) {}

	async fn closed(&mut self) -> Result<(), Self::Error> {
		Ok(())
	}
}

// === Adapter Error ===

#[derive(Debug, Clone)]
pub enum AdapterError {
	Closed,
}

impl std::fmt::Display for AdapterError {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		match self {
			AdapterError::Closed => write!(f, "adapter closed"),
		}
	}
}

impl std::error::Error for AdapterError {}

impl web_transport_trait::Error for AdapterError {
	fn session_error(&self) -> Option<(u32, String)> {
		None
	}
}

// === Adapter Send/Recv Enums ===

pub enum AdapterSend<S: web_transport_trait::Session> {
	Real(S::SendStream),
	Virtual(VirtualSendStream),
}

impl<S: web_transport_trait::Session> web_transport_trait::SendStream for AdapterSend<S> {
	type Error = AdapterError;

	async fn write(&mut self, buf: &[u8]) -> Result<usize, Self::Error> {
		match self {
			Self::Real(s) => s.write(buf).await.map_err(|_| AdapterError::Closed),
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
			Self::Real(s) => s.finish().map_err(|_| AdapterError::Closed),
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
			Self::Real(s) => s.closed().await.map_err(|_| AdapterError::Closed),
			Self::Virtual(s) => s.closed().await,
		}
	}
}

pub enum AdapterRecv<S: web_transport_trait::Session> {
	Real(S::RecvStream),
	Virtual(VirtualRecvStream),
}

impl<S: web_transport_trait::Session> web_transport_trait::RecvStream for AdapterRecv<S> {
	type Error = AdapterError;

	async fn read(&mut self, dst: &mut [u8]) -> Result<Option<usize>, Self::Error> {
		match self {
			Self::Real(s) => s.read(dst).await.map_err(|_| AdapterError::Closed),
			Self::Virtual(s) => s.read(dst).await,
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
			Self::Real(s) => s.closed().await.map_err(|_| AdapterError::Closed),
			Self::Virtual(s) => s.closed().await,
		}
	}
}

// === Control Stream Adapter ===

struct Shared {
	incoming_tx: mpsc::UnboundedSender<(VirtualSendStream, VirtualRecvStream)>,
	incoming_rx: tokio::sync::Mutex<mpsc::UnboundedReceiver<(VirtualSendStream, VirtualRecvStream)>>,

	/// Channel that VirtualSendStreams write to; the writer task reads from this.
	control_tx: mpsc::UnboundedSender<Vec<u8>>,

	/// Active virtual streams keyed by request_id.
	streams: Mutex<HashMap<RequestId, mpsc::UnboundedSender<Bytes>>>,

	/// Request ID allocation state.
	request_id_next: Mutex<RequestId>,
}

#[derive(Clone)]
pub struct ControlStreamAdapter<S: web_transport_trait::Session> {
	inner: S,
	shared: Arc<Shared>,
}

impl<S: web_transport_trait::Session> ControlStreamAdapter<S> {
	pub fn new(inner: S, control_tx: mpsc::UnboundedSender<Vec<u8>>, client: bool) -> Self {
		let (incoming_tx, incoming_rx) = mpsc::unbounded_channel();
		Self {
			inner,
			shared: Arc::new(Shared {
				incoming_tx,
				incoming_rx: tokio::sync::Mutex::new(incoming_rx),
				control_tx,
				streams: Mutex::new(HashMap::new()),
				request_id_next: Mutex::new(if client { RequestId(0) } else { RequestId(1) }),
			}),
		}
	}

	/// Run the dispatcher loop that reads control stream messages and routes them.
	pub async fn run(&self, mut reader: Reader<S::RecvStream, Version>, version: Version) -> Result<(), Error> {
		loop {
			let type_id: u64 = match reader.decode_maybe().await? {
				Some(id) => id,
				None => return Ok(()),
			};

			let size: u16 = reader.decode::<u16>().await?;
			tracing::trace!(type_id, size, "adapter: reading control message");

			let body = reader.read_exact(size as usize).await?;

			// Reconstruct raw message bytes: [type_id][size][body]
			let raw = encode_raw(type_id, size, &body, version);

			// Classify and route
			let route = classify(type_id, &body, version)?;
			tracing::trace!(?route, "adapter: classified message");

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
				Route::MaxRequestId(_max) => {
					// Handled by Control struct directly; adapter doesn't manage flow control.
					// Re-inject as raw bytes so the existing control dispatcher can handle it.
				}
				Route::GoAway => {
					return Err(Error::Unsupported);
				}
				Route::Ignore => {}
			}
		}
	}
}

impl<S: web_transport_trait::Session> web_transport_trait::Session for ControlStreamAdapter<S> {
	type SendStream = AdapterSend<S>;
	type RecvStream = AdapterRecv<S>;
	type Error = AdapterError;

	async fn accept_bi(&self) -> Result<(Self::SendStream, Self::RecvStream), Self::Error> {
		let mut rx = self.shared.incoming_rx.lock().await;
		match rx.recv().await {
			Some((send, recv)) => Ok((AdapterSend::Virtual(send), AdapterRecv::Virtual(recv))),
			None => Err(AdapterError::Closed),
		}
	}

	async fn open_bi(&self) -> Result<(Self::SendStream, Self::RecvStream), Self::Error> {
		let request_id = {
			let mut next = self.shared.request_id_next.lock().unwrap();
			next.increment()
		};

		let (follow_tx, follow_rx) = mpsc::unbounded_channel();
		let recv = VirtualRecvStream::new(Bytes::new(), follow_rx);
		let send = VirtualSendStream::new(self.shared.control_tx.clone());
		self.shared.streams.lock().unwrap().insert(request_id, follow_tx);
		Ok((AdapterSend::Virtual(send), AdapterRecv::Virtual(recv)))
	}

	async fn open_uni(&self) -> Result<Self::SendStream, Self::Error> {
		let s = self.inner.open_uni().await.map_err(|_| AdapterError::Closed)?;
		Ok(AdapterSend::Real(s))
	}

	async fn accept_uni(&self) -> Result<Self::RecvStream, Self::Error> {
		let s = self.inner.accept_uni().await.map_err(|_| AdapterError::Closed)?;
		Ok(AdapterRecv::Real(s))
	}

	fn send_datagram(&self, payload: Bytes) -> Result<(), Self::Error> {
		self.inner.send_datagram(payload).map_err(|_| AdapterError::Closed)
	}

	async fn recv_datagram(&self) -> Result<Bytes, Self::Error> {
		self.inner.recv_datagram().await.map_err(|_| AdapterError::Closed)
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
		AdapterError::Closed
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
	Ignore,
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

/// Classify a control message and extract its request_id for routing.
fn classify(type_id: u64, body: &Bytes, version: Version) -> Result<Route, Error> {
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
			Ok(Route::NewRequest(id))
		}
		ietf::TrackStatus::ID => {
			let id = decode_request_id(body, version)?;
			Ok(Route::NewRequest(id))
		}
		// SubscribeNamespace on control stream (v14/v15 only)
		ietf::SubscribeNamespace::ID => match version {
			Version::Draft14 | Version::Draft15 => {
				let id = decode_request_id(body, version)?;
				Ok(Route::NewRequest(id))
			}
			_ => Ok(Route::Ignore),
		},

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
			_ => Ok(Route::Ignore),
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
			_ => Ok(Route::Ignore),
		},
		// 0x08: PublishNamespaceError in v14 only
		ietf::PublishNamespaceError::ID => match version {
			Version::Draft14 => {
				let id = decode_request_id(body, version)?;
				Ok(Route::CloseStream(id))
			}
			_ => Ok(Route::Ignore),
		},
		// SubscribeNamespaceOk (v14 only)
		ietf::SubscribeNamespaceOk::ID => match version {
			Version::Draft14 => {
				let id = decode_request_id(body, version)?;
				Ok(Route::Response(id))
			}
			_ => Ok(Route::Ignore),
		},
		// SubscribeNamespaceError (v14 only)
		ietf::SubscribeNamespaceError::ID => match version {
			Version::Draft14 => {
				let id = decode_request_id(body, version)?;
				Ok(Route::CloseStream(id))
			}
			_ => Ok(Route::Ignore),
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
			// v14/v15: namespace-keyed, can't route by request_id
			_ => Ok(Route::Ignore),
		},
		ietf::PublishNamespaceCancel::ID => match version {
			Version::Draft16 => {
				let id = decode_request_id(body, version)?;
				Ok(Route::CloseStream(id))
			}
			// v14/v15: namespace-keyed
			_ => Ok(Route::Ignore),
		},
		ietf::UnsubscribeNamespace::ID => match version {
			Version::Draft14 | Version::Draft15 => {
				let id = decode_request_id(body, version)?;
				Ok(Route::CloseStream(id))
			}
			_ => Ok(Route::Ignore),
		},

		// Utility
		ietf::MaxRequestId::ID => {
			let id = decode_request_id(body, version)?;
			Ok(Route::MaxRequestId(id))
		}
		ietf::RequestsBlocked::ID => Ok(Route::Ignore),

		// Terminal
		ietf::GoAway::ID => Ok(Route::GoAway),

		_ => {
			tracing::warn!(type_id, "adapter: unknown message type");
			Err(Error::UnexpectedMessage)
		}
	}
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

	#[test]
	fn test_classify_subscribe_new_request() {
		let body = make_body_with_request_id(42, Version::Draft15);
		let route = classify(ietf::Subscribe::ID, &body, Version::Draft15).unwrap();
		assert!(matches!(route, Route::NewRequest(RequestId(42))));
	}

	#[test]
	fn test_classify_fetch_new_request() {
		let body = make_body_with_request_id(10, Version::Draft14);
		let route = classify(ietf::Fetch::ID, &body, Version::Draft14).unwrap();
		assert!(matches!(route, Route::NewRequest(RequestId(10))));
	}

	#[test]
	fn test_classify_publish_new_request() {
		let body = make_body_with_request_id(5, Version::Draft16);
		let route = classify(ietf::Publish::ID, &body, Version::Draft16).unwrap();
		assert!(matches!(route, Route::NewRequest(RequestId(5))));
	}

	#[test]
	fn test_classify_subscribe_ok_response() {
		let body = make_body_with_request_id(42, Version::Draft15);
		let route = classify(ietf::SubscribeOk::ID, &body, Version::Draft15).unwrap();
		assert!(matches!(route, Route::Response(RequestId(42))));
	}

	#[test]
	fn test_classify_request_error_v15_closes_stream() {
		// 0x05 is RequestError in v15+
		let body = make_body_with_request_id(7, Version::Draft15);
		let route = classify(ietf::SubscribeError::ID, &body, Version::Draft15).unwrap();
		assert!(matches!(route, Route::CloseStream(RequestId(7))));
	}

	#[test]
	fn test_classify_request_ok_v15_response() {
		// 0x07 is RequestOk in v15+
		let body = make_body_with_request_id(3, Version::Draft15);
		let route = classify(ietf::PublishNamespaceOk::ID, &body, Version::Draft15).unwrap();
		assert!(matches!(route, Route::Response(RequestId(3))));
	}

	#[test]
	fn test_classify_unsubscribe_closes_stream() {
		let body = make_body_with_request_id(99, Version::Draft14);
		let route = classify(ietf::Unsubscribe::ID, &body, Version::Draft14).unwrap();
		assert!(matches!(route, Route::CloseStream(RequestId(99))));
	}

	#[test]
	fn test_classify_subscribe_update_followup() {
		let body = make_body_with_request_id(10, Version::Draft15);
		let route = classify(ietf::SubscribeUpdate::ID, &body, Version::Draft15).unwrap();
		assert!(matches!(route, Route::FollowUp(RequestId(10))));
	}

	#[test]
	fn test_classify_goaway() {
		let body = Bytes::new();
		let route = classify(ietf::GoAway::ID, &body, Version::Draft14).unwrap();
		assert!(matches!(route, Route::GoAway));
	}

	#[test]
	fn test_classify_max_request_id() {
		let body = make_body_with_request_id(100, Version::Draft14);
		let route = classify(ietf::MaxRequestId::ID, &body, Version::Draft14).unwrap();
		assert!(matches!(route, Route::MaxRequestId(RequestId(100))));
	}

	#[test]
	fn test_classify_subscribe_namespace_v14_new_request() {
		let body = make_body_with_request_id(20, Version::Draft14);
		let route = classify(ietf::SubscribeNamespace::ID, &body, Version::Draft14).unwrap();
		assert!(matches!(route, Route::NewRequest(RequestId(20))));
	}

	#[test]
	fn test_classify_subscribe_namespace_v16_ignored() {
		let body = make_body_with_request_id(20, Version::Draft16);
		let route = classify(ietf::SubscribeNamespace::ID, &body, Version::Draft16).unwrap();
		assert!(matches!(route, Route::Ignore));
	}

	#[test]
	fn test_classify_unknown_message() {
		let body = Bytes::new();
		let result = classify(0xFF, &body, Version::Draft14);
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
		assert_eq!(data, b"hello");
	}
}
