use std::{
	io,
	pin::Pin,
	sync::Arc,
	task::{Context, Poll},
};

use bytes::{BufMut, Bytes};

use crate::quic::driver::recv;
use crate::quic::web::{CloseReason, ReadError, SessionError};

/// A stream that can be used to receive bytes. See [`crate::quic::driver::recv::Stream`].
///
/// Dropping it before the end stops it with code 0.
#[derive(Debug)]
pub struct RecvStream {
	/// Taken by [`Self::stop`], since `web_transport_trait` stops through `&mut self`.
	inner: Option<recv::Stream>,
	close: Arc<CloseReason>,
	id: crate::quic::driver::stream::Id,
}

impl RecvStream {
	pub(crate) fn new(mut stream: recv::Stream, close: Arc<CloseReason>) -> Self {
		// The driver's raw 0 is outside the HTTP/3 range, so a WebTransport peer can't decode it.
		stream.set_drop_stop(close.encode_stream_code(0));
		Self {
			id: stream.id(),
			inner: Some(stream),
			close,
		}
	}

	fn inner(&mut self) -> Result<&mut recv::Stream, ReadError> {
		self.inner.as_mut().ok_or(ReadError::ClosedStream)
	}

	/// Lease the stream's unreleased bytes. See [`recv::Stream::poll_lease`].
	pub fn poll_lease(&mut self, cx: &mut Context<'_>, want: usize) -> Poll<Result<recv::Lease<'_>, ReadError>> {
		let close = &self.close;
		match &mut self.inner {
			Some(inner) => inner.poll_lease(cx, want).map_err(|e| map_error(close, e)),
			None => Poll::Ready(Err(ReadError::ClosedStream)),
		}
	}

	/// Rank this stream for eviction. See [`recv::Stream::set_priority`].
	pub fn set_priority(&mut self, priority: i64) {
		if let Some(inner) = &mut self.inner {
			inner.set_priority(priority);
		}
	}

	/// Tell the other end to stop sending data with the given error code. See
	/// [`recv::Stream::stop`]. This is a u32 with WebTransport since it shares the error
	/// space with HTTP/3. A raw QUIC session sends the code as is. Does nothing once stopped.
	pub fn stop(&mut self, code: u32) {
		if let Some(inner) = self.inner.take() {
			inner.stop(self.close.encode_stream_code(code));
		}
	}

	/// Read some data into `buf` and return the amount read. See [`recv::Stream::read`].
	pub async fn read(&mut self, buf: &mut impl BufMut) -> Result<Option<usize>, ReadError> {
		let result = self.inner()?.read(buf).await;
		result.map_err(|e| map_error(&self.close, e))
	}

	/// Fill the entire buffer with data. See [`recv::Stream::read_exact`].
	pub async fn read_exact(&mut self, buf: &mut [u8]) -> Result<(), ReadError> {
		let result = self.inner()?.read_exact(buf).await;
		result.map_err(|e| map_error(&self.close, e))
	}

	/// Read a chunk of data from the stream. See [`recv::Stream::read_chunk`].
	pub async fn read_chunk(&mut self, max: usize) -> Result<Option<Bytes>, ReadError> {
		let result = self.inner()?.read_chunk(max).await;
		result.map_err(|e| map_error(&self.close, e))
	}

	/// Read until the end of the stream or the limit is hit. See [`recv::Stream::read_to_end`].
	pub async fn read_to_end(&mut self, limit: usize) -> Result<Vec<u8>, ReadError> {
		let result = self.inner()?.read_to_end(limit).await;
		result.map_err(|e| map_error(&self.close, e))
	}

	/// Block until the stream has been reset and return the error code. See
	/// [`recv::Stream::received_reset`].
	///
	/// Returns the stream's reset code or its session error.
	pub async fn received_reset(&mut self) -> Result<Option<u32>, SessionError> {
		let Some(inner) = &mut self.inner else {
			return Ok(None);
		};
		match inner.received_reset().await {
			Ok(None) => Ok(None),
			Ok(Some(code)) => match self.close.session_gone(code) {
				Some(err) => Err(err),
				None => Ok(self.close.decode_stream_code(code)),
			},
			Err(err) => match map_error(&self.close, err) {
				ReadError::SessionError(err) => Err(err),
				err => unreachable!("only the connection fails a reset wait: {err}"),
			},
		}
	}

	/// Return the underlying QUIC stream ID.
	///
	/// > **Warning**
	/// >
	/// > WebTransport sessions share the QUIC connection with HTTP/3 and potentially other
	/// > sessions.
	/// > The [crate::quic::driver::stream::Id::index] might not increment by 1 like expected when using raw QUIC.
	/// > This is why the Javascript WebTransport API does not expose the Stream ID.
	pub fn quic_id(&self) -> crate::quic::driver::stream::Id {
		self.id
	}

	// We purposely don't expose the 0RTT because it's not valid with WebTransport
}

/// Decode the peer's reset code, and report a session error, or a reset for a session
/// that is gone, as the session's close reason.
fn map_error(close: &CloseReason, e: recv::Error) -> ReadError {
	match e {
		recv::Error::Reset(code) => match close.decode_stream_code(code) {
			Some(code) => ReadError::Reset(code),
			None => match close.session_gone(code) {
				Some(err) => ReadError::SessionError(err),
				None => ReadError::InvalidReset(code),
			},
		},
		e => match ReadError::from(e) {
			ReadError::SessionError(e) => ReadError::SessionError(close.map(e)),
			e => e,
		},
	}
}

impl tokio::io::AsyncRead for RecvStream {
	fn poll_read(mut self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &mut tokio::io::ReadBuf) -> Poll<io::Result<()>> {
		match &mut self.inner {
			Some(inner) => Pin::new(inner).poll_read(cx, buf),
			None => Poll::Ready(Err(io::ErrorKind::NotConnected.into())),
		}
	}
}

impl web_transport_trait::RecvStream for RecvStream {
	type Error = ReadError;

	fn stop(&mut self, code: u32) {
		Self::stop(self, code);
	}

	async fn read(&mut self, mut dst: &mut [u8]) -> Result<Option<usize>, Self::Error> {
		self.read(&mut dst).await
	}

	async fn read_chunk(&mut self, max: usize) -> Result<Option<Bytes>, Self::Error> {
		self.read_chunk(max).await
	}

	async fn closed(&mut self) -> Result<(), Self::Error> {
		self.received_reset().await?;
		Ok(())
	}
}
