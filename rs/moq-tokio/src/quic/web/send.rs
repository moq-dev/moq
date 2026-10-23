use std::{
	io,
	pin::Pin,
	sync::Arc,
	task::{Context, Poll},
};

use bytes::{Buf, Bytes};

use crate::quic::web::{CloseReason, ClosedStream, SessionError, WriteError};

/// A stream that can be used to send bytes. See [`crate::quic::driver::SendStream`].
///
/// Dropping it before [`finish`](Self::finish) resets it with code 0, still delivering the
/// WebTransport header.
///
/// This wrapper is mainly needed for error codes, which is unfortunate.
/// WebTransport uses u32 error codes and they're mapped in a reserved HTTP/3 error space.
#[derive(Debug)]
pub struct SendStream {
	stream: crate::quic::driver::SendStream,
	close: Arc<CloseReason>,
	/// The end of the WebTransport header this side wrote first, which a reset must still deliver.
	header: Option<u64>,
}

impl SendStream {
	pub(crate) fn new(
		mut stream: crate::quic::driver::SendStream,
		close: Arc<CloseReason>,
		header: Option<u64>,
	) -> Self {
		stream.set_drop_reset(header.unwrap_or(0), close.encode_stream_code(0));
		Self { stream, close, header }
	}

	/// Decode the peer's stop code, and report a session error, or a stop for a session
	/// that is gone, as the session's close reason.
	fn map_error(&self, e: crate::quic::driver::WriteError) -> WriteError {
		match e {
			crate::quic::driver::WriteError::Stopped(code) => match self.close.decode_stream_code(code) {
				Some(code) => WriteError::Stopped(code),
				None => match self.close.session_gone(code) {
					Some(err) => WriteError::SessionError(err),
					None => WriteError::InvalidStopped(code),
				},
			},
			e => match WriteError::from(e) {
				WriteError::SessionError(e) => WriteError::SessionError(self.close.map(e)),
				e => e,
			},
		}
	}

	/// Abruptly reset the stream with the provided error code. See [`crate::quic::driver::SendStream::reset`].
	/// This is a u32 with WebTransport because we share the error space with HTTP/3.
	/// A raw QUIC session sends the code as is.
	///
	/// The WebTransport header still reaches the peer, so it can tell which session the reset
	/// belongs to.
	pub fn reset(&mut self, code: u32) -> Result<(), ClosedStream> {
		let code = self.close.encode_stream_code(code);
		match self.header.take() {
			Some(header) => self.stream.reset_at(header, code).map_err(Into::into),
			None => self.stream.reset(code).map_err(Into::into),
		}
	}

	/// Wait until the stream has been stopped and return the error code. See
	/// [`crate::quic::driver::SendStream::stopped`].
	///
	/// Unlike the driver, this returns None if the code is not a valid WebTransport error code.
	/// Also unlike the driver, this returns a SessionError, not a StoppedError, because 0-RTT is not
	/// supported.
	pub async fn stopped(&self) -> Result<Option<u32>, SessionError> {
		match self.stream.stopped().await {
			Ok(Some(code)) => match self.close.session_gone(code) {
				Some(err) => Err(err),
				None => Ok(self.close.decode_stream_code(code)),
			},
			Ok(None) => Ok(None),
			Err(crate::quic::driver::StoppedError::ConnectionLost(conn_err)) => Err(self.close.map(conn_err.into())),
			Err(crate::quic::driver::StoppedError::ZeroRttRejected) => unreachable!("0-RTT not supported"),
		}
	}

	// Unfortunately, we have to wrap WriteError for a bunch of functions.

	/// Write some data to the stream, returning the size written. See [`crate::quic::driver::SendStream::write`].
	pub async fn write(&mut self, buf: &[u8]) -> Result<usize, WriteError> {
		self.stream.write(buf).await.map_err(|e| self.map_error(e))
	}

	/// Write all of the data to the stream. See [`crate::quic::driver::SendStream::write_all`].
	pub async fn write_all(&mut self, buf: &[u8]) -> Result<(), WriteError> {
		self.stream.write_all(buf).await.map_err(|e| self.map_error(e))
	}

	/// Write a chunk of data to the stream. See [`crate::quic::driver::SendStream::write_chunk`].
	pub async fn write_chunk(&mut self, buf: Bytes) -> Result<(), WriteError> {
		self.stream.write_chunk(buf).await.map_err(|e| self.map_error(e))
	}

	/// Write all of the chunks of data to the stream. See [`crate::quic::driver::SendStream::write_all_chunks`].
	pub async fn write_all_chunks(&mut self, bufs: &mut [Bytes]) -> Result<(), WriteError> {
		self.stream.write_all_chunks(bufs).await.map_err(|e| self.map_error(e))
	}

	/// Mark the stream as finished, such that no more data can be written. See
	/// [`crate::quic::driver::SendStream::finish`].
	pub fn finish(&mut self) -> Result<(), ClosedStream> {
		self.stream.finish().map_err(Into::into)
	}

	/// Set the priority within the stream's send group, or session-wide when ungrouped.
	pub fn set_priority(&self, priority: i64) -> Result<(), ClosedStream> {
		self.stream.set_priority(priority).map_err(Into::into)
	}

	/// Get the stream's priority. See [`crate::quic::driver::SendStream::priority`].
	pub fn priority(&self) -> Result<i64, ClosedStream> {
		self.stream.priority().map_err(Into::into)
	}

	/// Move the stream into a send group, or with `None` back out. See
	/// [`crate::quic::driver::SendStream::set_group`].
	pub fn set_group(&self, group: Option<&crate::quic::driver::SendGroup>) -> Result<(), ClosedStream> {
		self.stream.set_group(group).map_err(Into::into)
	}

	/// Return the underlying QUIC stream ID.
	///
	/// > **Warning**
	/// >
	/// > WebTransport sessions share the QUIC connection with HTTP/3 and potentially other
	/// > sessions.
	/// > The [crate::quic::driver::StreamId::index] might not increment by 1 like expected when using raw QUIC.
	/// > This is why the Javascript WebTransport API does not expose the Stream ID.
	pub fn quic_id(&self) -> crate::quic::driver::StreamId {
		self.stream.id()
	}
}

impl tokio::io::AsyncWrite for SendStream {
	fn poll_write(mut self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &[u8]) -> Poll<io::Result<usize>> {
		// We have to use this syntax because the driver has its own poll_write method.
		tokio::io::AsyncWrite::poll_write(Pin::new(&mut self.stream), cx, buf)
	}

	fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context) -> Poll<io::Result<()>> {
		Pin::new(&mut self.stream).poll_flush(cx)
	}

	fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context) -> Poll<io::Result<()>> {
		Pin::new(&mut self.stream).poll_shutdown(cx)
	}
}

impl web_transport_trait::SendStream for SendStream {
	type Error = WriteError;

	fn set_priority(&mut self, order: i32) {
		Self::set_priority(self, order.into()).ok();
	}

	fn reset(&mut self, code: u32) {
		Self::reset(self, code).ok();
	}

	fn finish(&mut self) -> Result<(), Self::Error> {
		Self::finish(self).map_err(|_| WriteError::ClosedStream)
	}

	async fn write(&mut self, buf: &[u8]) -> Result<usize, Self::Error> {
		Self::write(self, buf).await
	}

	async fn write_buf<B: Buf + Send>(&mut self, buf: &mut B) -> Result<usize, Self::Error> {
		// This can avoid making a copy when Buf is Bytes, as the driver will allocate anyway.
		let size = buf.chunk().len();
		let chunk = buf.copy_to_bytes(size);
		self.write_chunk(chunk).await?;
		Ok(size)
	}

	async fn write_chunk(&mut self, chunk: Bytes) -> Result<(), Self::Error> {
		self.write_chunk(chunk).await
	}

	async fn closed(&mut self) -> Result<(), Self::Error> {
		match self.stopped().await? {
			Some(code) => Err(WriteError::Stopped(code)),
			None => Ok(()),
		}
	}
}
