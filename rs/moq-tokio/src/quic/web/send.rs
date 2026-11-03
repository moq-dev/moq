//! Outgoing WebTransport streams, their finish handles, and errors.

use std::{
	io,
	pin::Pin,
	sync::Arc,
	task::{Context, Poll},
};

use bytes::Buf;

use crate::quic::driver::send;
use crate::quic::web::{CloseReason, SessionError};

/// A stream that can be used to send bytes. See [`crate::quic::driver::send::Stream`].
///
/// Dropping it before [`finish`](Self::finish) resets it with code 0, still delivering the
/// WebTransport header.
///
/// This wrapper is mainly needed for error codes, which is unfortunate.
/// WebTransport uses u32 error codes and they're mapped in a reserved HTTP/3 error space.
#[derive(Debug)]
pub struct Stream {
	state: State,
	close: Arc<CloseReason>,
	/// The end of the WebTransport header this side wrote first, which a reset must still deliver.
	header: Option<send::Offset>,
	id: crate::quic::driver::stream::Id,
}

/// The mutable terminal state required by moq-net's poll trait.
#[derive(Debug)]
enum State {
	Open(send::Stream),
	Finished(send::Finished),
	/// Reset, with the peer's stop code if it arrived first.
	Reset(Result<Option<crate::quic::driver::VarInt>, send::Error>),
}

impl Stream {
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
	fn map_error(&self, e: send::Error) -> Error {
		match e {
			send::Error::Stopped(code) => match self.close.decode_stream_code(code) {
				Some(code) => Error::Stopped(code),
				None => match self.close.session_gone(code) {
					Some(err) => Error::SessionError(err),
					None => Error::InvalidStopped(code),
				},
			},
			e => match Error::from(e) {
				Error::SessionError(e) => Error::SessionError(self.close.map(e)),
				e => e,
			},
		}
	}

	fn open(&mut self) -> Result<&mut send::Stream, Error> {
		match &mut self.state {
			State::Open(stream) => Ok(stream),
			_ => Err(Error::ClosedStream),
		}
	}

	/// Abruptly reset the stream with the provided error code. See [`send::Stream::reset`].
	/// This is a u32 with WebTransport because we share the error space with HTTP/3.
	/// A raw QUIC session sends the code as is. Does nothing once already reset.
	///
	/// The WebTransport header still reaches the peer, so it can tell which session the reset
	/// belongs to.
	fn reset_mut(&mut self, code: u32) {
		let code = self.close.encode_stream_code(code);
		let stopped = match &self.state {
			State::Open(stream) => stream.poll_closed(&kio::Waiter::noop()),
			State::Finished(stream) => stream.poll_closed(&kio::Waiter::noop()),
			State::Reset(_) => return,
		};
		let stopped = match stopped {
			Poll::Ready(result) => result,
			Poll::Pending => Ok(None),
		};
		match std::mem::replace(&mut self.state, State::Reset(stopped)) {
			State::Open(stream) => match self.header {
				Some(header) => stream.reset_at(header, code),
				None => stream.reset(code),
			},
			State::Finished(stream) => match self.header {
				Some(header) => stream.reset_at(header, code),
				None => stream.reset(code),
			},
			State::Reset(_) => unreachable!(),
		}
	}

	/// Poll until all data was acknowledged, the peer stopped the stream, or the session closed.
	pub fn poll_closed(&self, waiter: &kio::Waiter) -> Poll<Result<Option<u32>, Error>> {
		let result = match &self.state {
			State::Open(stream) => stream.poll_closed(waiter),
			State::Finished(stream) => stream.poll_closed(waiter),
			State::Reset(result) => Poll::Ready(result.clone()),
		};
		result.map(|result| match result {
			Ok(None) => Ok(None),
			Ok(Some(code)) => match self.map_error(send::Error::Stopped(code)) {
				Error::Stopped(code) => Ok(Some(code)),
				err => Err(err),
			},
			Err(err) => Err(self.map_error(err)),
		})
	}

	/// Wait until all data was acknowledged, the peer stopped the stream, or the session closed.
	pub async fn stopped(&self) -> Result<Option<u32>, Error> {
		kio::wait(|w| self.poll_closed(w)).await
	}

	/// Finish sending, retaining a handle to acknowledgements and peer cancellation.
	pub fn finish(mut self) -> Finished {
		self.finish_mut();
		Finished {
			stream: self,
			park: Default::default(),
		}
	}

	/// Reset the stream while still delivering its WebTransport header.
	pub fn reset(mut self, code: u32) {
		self.reset_mut(code);
	}

	/// Write from `buf`, returning the size written. See [`send::Stream::write`].
	pub async fn write(&mut self, buf: &mut impl Buf) -> Result<usize, Error> {
		let result = self.open()?.write(buf).await;
		result.map_err(|e| self.map_error(e))
	}

	/// Write all of `buf` to the stream. See [`send::Stream::write_all`].
	pub async fn write_all(&mut self, buf: impl Buf) -> Result<(), Error> {
		let result = self.open()?.write_all(buf).await;
		result.map_err(|e| self.map_error(e))
	}

	/// Mark the stream as finished, such that no more data can be written. See
	/// [`send::Stream::finish`]. Does nothing if it was already finished or reset.
	fn finish_mut(&mut self) {
		if matches!(self.state, State::Open(_)) {
			let State::Open(stream) = std::mem::replace(&mut self.state, State::Reset(Ok(None))) else {
				unreachable!()
			};
			self.state = State::Finished(stream.finish());
		}
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
	pub fn id(&self) -> crate::quic::driver::stream::Id {
		self.id
	}
}

impl tokio::io::AsyncWrite for Stream {
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
		self.finish_mut();
		Poll::Ready(Ok(()))
	}
}

impl moq_net::transport::poll::SendStream for Stream {
	type Error = Error;
	type Group = send::Group;

	fn poll_write(&mut self, waiter: &kio::Waiter, mut buf: &[u8]) -> Poll<Result<usize, Self::Error>> {
		self.poll_write_buf(waiter, &mut buf)
	}
	fn poll_write_buf<B: Buf>(&mut self, waiter: &kio::Waiter, buf: &mut B) -> Poll<Result<usize, Self::Error>> {
		let result = self.open()?.poll_write_buf(&waiter.context(), buf);
		result.map_err(|e| self.map_error(e))
	}
	fn set_priority(&mut self, priority: i64) {
		Self::set_priority(self, priority);
	}
	fn set_group(&mut self, group: &Self::Group) {
		Self::set_group(self, Some(group));
	}
	fn finish(&mut self) -> Result<(), Self::Error> {
		Self::finish_mut(self);
		Ok(())
	}
	fn reset(&mut self, code: u32) {
		Self::reset_mut(self, code);
	}
	fn poll_closed(&self, waiter: &kio::Waiter) -> Poll<Result<(), Self::Error>> {
		Self::poll_closed(self, waiter).map(|result| match result {
			Ok(None) => Ok(()),
			Ok(Some(code)) => Err(Error::Stopped(code)),
			Err(err) => Err(err),
		})
	}
}

impl moq_net::transport::poll::SendGroup for send::Group {
	fn set_priority(&mut self, priority: i64) {
		Self::set_priority(self, priority);
	}
}

/// A finished stream whose data keeps transmitting until acknowledged or stopped.
#[derive(Debug)]
pub struct Finished {
	stream: Stream,
	park: kio::Park,
}
impl Finished {
	/// Poll until the peer acknowledges or stops the stream, or the session closes.
	pub fn poll_closed(&self, waiter: &kio::Waiter) -> Poll<Result<Option<u32>, Error>> {
		self.stream.poll_closed(waiter)
	}
	/// Wait until the peer acknowledges or stops the stream, or the session closes.
	pub async fn stopped(&self) -> Result<Option<u32>, Error> {
		self.stream.stopped().await
	}
	/// Reset the stream while still delivering its WebTransport header.
	pub fn reset(self, code: u32) {
		self.stream.reset(code);
	}
	/// Set the stream's priority within its send group.
	pub fn set_priority(&mut self, priority: i64) {
		self.stream.set_priority(priority);
	}
	/// Return the stream's priority within its send group.
	pub fn priority(&self) -> i64 {
		self.stream.priority()
	}
	/// Move the stream to a send group, or out with `None`.
	pub fn set_group(&mut self, group: Option<&send::Group>) {
		self.stream.set_group(group);
	}
	/// Return the underlying QUIC stream ID.
	pub fn id(&self) -> crate::quic::driver::stream::Id {
		self.stream.id()
	}
}
impl Future for Finished {
	type Output = Result<Option<u32>, Error>;
	fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
		let this = &mut *self;
		this.stream.poll_closed(this.park.hold(cx))
	}
}

/// An error when writing to [`crate::quic::web::send::Stream`]. Similar to [`crate::quic::driver::send::Error`].
#[derive(Clone, thiserror::Error, Debug)]
#[non_exhaustive]
pub enum Error {
	/// The peer stopped accepting this stream with its application code.
	#[error("STOP_SENDING: {0}")]
	Stopped(u32),

	/// The peer stopped the stream with a code outside the WebTransport registry.
	#[error("invalid STOP_SENDING: {0}")]
	InvalidStopped(crate::quic::driver::VarInt),

	/// The session carrying the stream failed.
	#[error("session error: {0}")]
	SessionError(#[from] SessionError),

	/// The stream was already finished or reset.
	#[error("stream closed")]
	ClosedStream,
}

impl From<crate::quic::driver::send::Error> for Error {
	fn from(e: crate::quic::driver::send::Error) -> Self {
		match e {
			crate::quic::driver::send::Error::Stopped(code) => {
				match web_transport_proto::error_from_http3(code.into_inner()) {
					Some(code) => Error::Stopped(code),
					None => Error::InvalidStopped(code),
				}
			}
			crate::quic::driver::send::Error::Connection(e) => Error::SessionError(e.into()),
			// Only datagrams fail otherwise, and they report a session error.
			e => Error::SessionError(e.into()),
		}
	}
}

impl moq_net::transport::Error for Error {
	fn close_kind(&self) -> moq_net::transport::CloseKind {
		match self {
			Self::SessionError(e) => e.close_kind(),
			_ => moq_net::transport::CloseKind::Transport,
		}
	}
	fn session_error(&self) -> Option<(u32, String)> {
		if let Error::SessionError(e) = self {
			return e.session_error();
		}

		None
	}

	fn stream_error(&self) -> Option<u32> {
		match self {
			Error::Stopped(code) => Some(*code),
			_ => None,
		}
	}
}

/// The driver's send groups and scheduling options.
pub use crate::quic::driver::send::{Group, Options};
