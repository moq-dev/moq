use std::{
	io,
	pin::Pin,
	sync::Arc,
	task::{Context, Poll},
};

use bytes::{Buf, Bytes};
use futures::FutureExt;

use crate::quic::driver::send;
use crate::quic::web::{CloseReason, SessionError, WriteError};

/// A stream that can be used to send bytes. See [`crate::quic::driver::send::Stream`].
///
/// Dropping it before [`finish`](Self::finish) resets it with code 0, still delivering the
/// WebTransport header.
///
/// This wrapper is mainly needed for error codes, which is unfortunate.
/// WebTransport uses u32 error codes and they're mapped in a reserved HTTP/3 error space.
#[derive(Debug)]
pub struct SendStream {
	state: State,
	close: Arc<CloseReason>,
	/// The end of the WebTransport header this side wrote first, which a reset must still deliver.
	header: Option<send::Offset>,
	id: crate::quic::driver::stream::Id,
}

/// Where the stream is, since `web_transport_trait` finishes and resets through `&mut self`.
#[derive(Debug)]
enum State {
	Open(send::Stream),
	Finished(send::Finished),
	/// Reset, with the peer's stop code if it arrived first.
	Reset(Result<Option<crate::quic::driver::VarInt>, send::Error>),
}

impl SendStream {
	pub(crate) fn new(mut stream: send::Stream, close: Arc<CloseReason>, header: Option<send::Offset>) -> Self {
		stream.set_drop_reset(header, close.encode_stream_code(0));
		Self {
			id: stream.id(),
			state: State::Open(stream),
			close,
			header,
		}
	}

	/// Decode the peer's stop code, and report a session error, or a stop for a session
	/// that is gone, as the session's close reason.
	fn map_error(&self, e: send::Error) -> WriteError {
		match e {
			send::Error::Stopped(code) => match self.close.decode_stream_code(code) {
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

	fn open(&mut self) -> Result<&mut send::Stream, WriteError> {
		match &mut self.state {
			State::Open(stream) => Ok(stream),
			_ => Err(WriteError::ClosedStream),
		}
	}

	/// Abruptly reset the stream with the provided error code. See [`send::Stream::reset`].
	/// This is a u32 with WebTransport because we share the error space with HTTP/3.
	/// A raw QUIC session sends the code as is. Does nothing once already reset.
	///
	/// The WebTransport header still reaches the peer, so it can tell which session the reset
	/// belongs to.
	pub fn reset(&mut self, code: u32) {
		let code = self.close.encode_stream_code(code);
		let header = self.header;
		// The peer's stop code still wins if it arrived first, which only a `stopped` future
		// taken before the reset learns, and the reset settles it.
		let stopped = match std::mem::replace(&mut self.state, State::Reset(Ok(None))) {
			State::Open(stream) => {
				let stopped = stream.stopped();
				match header {
					Some(header) => stream.reset_at(header, code),
					None => stream.reset(code),
				}
				stopped.now_or_never()
			}
			State::Finished(stream) => {
				let stopped = stream.stopped();
				match header {
					Some(header) => stream.reset_at(header, code),
					None => stream.reset(code),
				}
				stopped.now_or_never()
			}
			state @ State::Reset(_) => {
				self.state = state;
				return;
			}
		};
		self.state = State::Reset(stopped.expect("a reset settles its stopped future"));
	}

	/// Wait until the stream has been stopped and return the error code. See
	/// [`send::Finished::stopped`].
	///
	/// Unlike the driver, this returns None if the code is not a valid WebTransport error code.
	pub async fn stopped(&self) -> Result<Option<u32>, SessionError> {
		let stopped = match &self.state {
			State::Open(stream) => stream.stopped().await,
			State::Finished(stream) => stream.stopped().await,
			State::Reset(stopped) => stopped.clone(),
		};
		match stopped {
			Ok(Some(code)) => match self.close.session_gone(code) {
				Some(err) => Err(err),
				None => Ok(self.close.decode_stream_code(code)),
			},
			Ok(None) => Ok(None),
			Err(err) => match WriteError::from(err) {
				WriteError::SessionError(err) => Err(self.close.map(err)),
				err => unreachable!("only the connection fails a stopped wait: {err}"),
			},
		}
	}

	/// Write from `buf`, returning the size written. See [`send::Stream::write`].
	pub async fn write(&mut self, buf: &mut impl Buf) -> Result<usize, WriteError> {
		let result = self.open()?.write(buf).await;
		result.map_err(|e| self.map_error(e))
	}

	/// Write all of `buf` to the stream. See [`send::Stream::write_all`].
	pub async fn write_all(&mut self, buf: impl Buf) -> Result<(), WriteError> {
		let result = self.open()?.write_all(buf).await;
		result.map_err(|e| self.map_error(e))
	}

	/// Mark the stream as finished, such that no more data can be written. See
	/// [`send::Stream::finish`]. Fails if it was already finished or reset.
	pub fn finish(&mut self) -> Result<(), WriteError> {
		self.open()?;
		let State::Open(stream) = std::mem::replace(&mut self.state, State::Reset(Ok(None))) else {
			unreachable!("checked above");
		};
		self.state = State::Finished(stream.finish());
		Ok(())
	}

	/// Set the priority within the stream's send group, or session-wide when ungrouped.
	pub fn set_priority(&mut self, priority: i64) {
		match &mut self.state {
			State::Open(stream) => stream.set_priority(priority),
			State::Finished(stream) => stream.set_priority(priority),
			State::Reset(_) => {}
		}
	}

	/// The stream's priority. See [`send::Stream::priority`].
	pub fn priority(&self) -> i64 {
		match &self.state {
			State::Open(stream) => stream.priority(),
			State::Finished(stream) => stream.priority(),
			State::Reset(_) => 0,
		}
	}

	/// Move the stream into a send group, or with `None` back out. See [`send::Stream::set_group`].
	pub fn set_group(&mut self, group: Option<&send::Group>) {
		match &mut self.state {
			State::Open(stream) => stream.set_group(group),
			State::Finished(stream) => stream.set_group(group),
			State::Reset(_) => {}
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
}

impl tokio::io::AsyncWrite for SendStream {
	fn poll_write(mut self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &[u8]) -> Poll<io::Result<usize>> {
		match &mut self.state {
			State::Open(stream) => Pin::new(stream).poll_write(cx, buf),
			_ => Poll::Ready(Err(io::ErrorKind::BrokenPipe.into())),
		}
	}

	fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context) -> Poll<io::Result<()>> {
		Poll::Ready(Ok(()))
	}

	fn poll_shutdown(mut self: Pin<&mut Self>, _cx: &mut Context) -> Poll<io::Result<()>> {
		// Already finished or reset, so there is nothing left to shut.
		let _ = self.finish();
		Poll::Ready(Ok(()))
	}
}

impl web_transport_trait::SendStream for SendStream {
	type Error = WriteError;

	fn set_priority(&mut self, order: i32) {
		Self::set_priority(self, order.into());
	}

	fn reset(&mut self, code: u32) {
		Self::reset(self, code);
	}

	fn finish(&mut self) -> Result<(), Self::Error> {
		Self::finish(self)
	}

	async fn write(&mut self, mut buf: &[u8]) -> Result<usize, Self::Error> {
		Self::write(self, &mut buf).await
	}

	async fn write_buf<B: Buf + Send>(&mut self, buf: &mut B) -> Result<usize, Self::Error> {
		Self::write(self, buf).await
	}

	async fn write_chunk(&mut self, chunk: Bytes) -> Result<(), Self::Error> {
		self.write_all(chunk).await
	}

	async fn closed(&mut self) -> Result<(), Self::Error> {
		match self.stopped().await? {
			Some(code) => Err(WriteError::Stopped(code)),
			None => Ok(()),
		}
	}
}
