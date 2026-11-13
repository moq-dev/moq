//! Outgoing WebTransport streams, their finish handles, and errors.

use std::{
	pin::Pin,
	rc::Rc,
	task::{Context, Poll},
};

use bytes::{Buf, Bytes};

use super::Handles;
use crate::quic::driver::send;

/// A stream that can be used to send bytes. See [`crate::quic::send::Stream`].
///
/// A stream this side opened writes its WebTransport header ahead of its first data, so opening
/// one waits only for the peer's stream credit. Dropping it before [`finish`](Self::finish)
/// resets it with code 0, or WT_SESSION_GONE once the session closed, still delivering the header
/// once written.
///
/// This wrapper is mainly needed for error codes, which is unfortunate.
/// WebTransport uses u32 error codes and they're mapped in a reserved HTTP/3 error space.
#[derive(Debug)]
pub struct Stream {
	state: State,
	close: Rc<Handles>,
	/// The WebTransport header still to write ahead of any data, empty once written or for a
	/// stream without one.
	unwritten: Bytes,
	/// The end of the WebTransport header once written, which a reset must still deliver.
	header: Option<send::Offset>,
	id: crate::quic::stream::Id,
}

/// The mutable terminal state required by moq-net's poll trait.
#[derive(Debug)]
enum State {
	Open(send::Stream),
	Finished(send::Finished),
	/// Finished before its header fit, so it was reset with nothing sent.
	Unsent,
	/// Reset, with the peer's stop code if it arrived first.
	Reset(Result<Option<crate::quic::VarInt>, send::Error>),
}

impl Stream {
	/// Wrap `stream`, writing `header` ahead of its data: a WebTransport stream this side
	/// opened, or empty for one without a header.
	pub(super) fn new(stream: send::Stream, close: &Rc<Handles>, header: Bytes) -> Self {
		let mut this = Self {
			id: stream.id(),
			state: State::Open(stream),
			close: close.clone(),
			unwritten: header,
			header: None,
		};
		// Write the header now when it fits, so the peer can accept the stream before its
		// first data. Whatever does not fit waits for that data; a failure resurfaces there.
		let _ = this.poll_header(&kio::Waiter::noop());
		this
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
	/// A WebTransport header already written still reaches the peer, so it can tell which
	/// session the reset belongs to.
	fn reset_mut(&mut self, code: u32) {
		let code = self.close.encode_stream_code(code);
		// The connection is driven on this thread, so no STOP_SENDING lands between this check
		// and the reset: what the stream reports now is what the reset settles.
		let stopped = match &self.state {
			State::Open(stream) => stream.poll_closed(&kio::Waiter::noop()),
			State::Finished(stream) => stream.poll_closed(&kio::Waiter::noop()),
			State::Reset(_) | State::Unsent => return,
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
			State::Reset(_) | State::Unsent => unreachable!(),
		}
	}

	/// Resolves once all data was acknowledged, the peer stopped the stream (with its code), or the
	/// session closed.
	#[kio::wrapper]
	pub fn poll_closed(&self, waiter: &kio::Waiter) -> Poll<Result<Option<u32>, Error>> {
		let result = match &self.state {
			State::Open(stream) => stream.poll_closed(waiter),
			State::Finished(stream) => stream.poll_closed(waiter),
			State::Reset(result) => Poll::Ready(result.clone()),
			State::Unsent => return Poll::Ready(Err(Error::Unsent)),
		};
		match std::task::ready!(result) {
			Ok(None) => Poll::Ready(Ok(None)),
			Ok(Some(code)) => poll_error(&self.close, waiter, send::Error::Stopped(code)).map(|err| match err {
				Error::Stopped(code) => Ok(Some(code)),
				err => Err(err),
			}),
			Err(err) => poll_error(&self.close, waiter, err).map(Err),
		}
	}

	/// Finish sending, retaining a handle to acknowledgements and peer cancellation.
	///
	/// A stream whose WebTransport header never fit is reset instead, and the handle fails
	/// with [`Error::Unsent`].
	pub fn finish(mut self) -> Finished {
		let _ = self.finish_mut();
		Finished {
			stream: self,
			park: Default::default(),
		}
	}

	/// Reset the stream while still delivering its WebTransport header.
	pub fn reset(mut self, code: u32) {
		self.reset_mut(code);
	}

	/// Write what is left of the WebTransport header, ready once none is left.
	fn poll_header(&mut self, waiter: &kio::Waiter) -> Poll<Result<(), Error>> {
		if !self.unwritten.has_remaining() {
			return Poll::Ready(Ok(()));
		}
		let State::Open(stream) = &mut self.state else {
			return Poll::Ready(Err(Error::ClosedStream));
		};
		while self.unwritten.has_remaining() {
			if let Err(err) = std::task::ready!(stream.poll_write_buf(waiter, &mut self.unwritten)) {
				return poll_error(&self.close, waiter, err).map(Err);
			}
		}
		let offset = stream.offset();
		self.header = Some(offset);
		Poll::Ready(Ok(()))
	}

	/// Write from `buf` after the header, returning the size written.
	fn poll_write_buf(&mut self, waiter: &kio::Waiter, buf: &mut impl Buf) -> Poll<Result<usize, Error>> {
		std::task::ready!(self.poll_header(waiter))?;
		match std::task::ready!(self.open()?.poll_write_buf(waiter, buf)) {
			Ok(n) => Poll::Ready(Ok(n)),
			Err(e) => poll_error(&self.close, waiter, e).map(Err),
		}
	}

	/// Write from `buf`, returning the size written. See [`send::Stream::write`].
	pub async fn write(&mut self, buf: &mut impl Buf) -> Result<usize, Error> {
		kio::wait(|waiter| self.poll_write_buf(waiter, buf)).await
	}

	/// Write all of `buf` to the stream. See [`send::Stream::write_all`].
	pub async fn write_all(&mut self, mut buf: impl Buf) -> Result<(), Error> {
		while buf.has_remaining() {
			self.write(&mut buf).await?;
		}
		Ok(())
	}

	/// Mark the stream as finished, such that no more data can be written. See
	/// [`send::Stream::finish`]. Does nothing if it was already finished or reset.
	///
	/// A stream whose header still does not fit could never deliver its FIN, so it is reset
	/// instead, failing with [`Error::Unsent`] here and from `poll_closed`.
	fn finish_mut(&mut self) -> Result<(), Error> {
		if !matches!(self.state, State::Open(_)) {
			return Ok(());
		}
		match self.poll_header(&kio::Waiter::noop()) {
			Poll::Ready(Ok(())) => {}
			Poll::Ready(Err(err)) => return Err(err),
			Poll::Pending => {
				self.reset_mut(0);
				self.state = State::Unsent;
				return Err(Error::Unsent);
			}
		}
		let State::Open(stream) = std::mem::replace(&mut self.state, State::Reset(Ok(None))) else {
			unreachable!()
		};
		self.state = State::Finished(stream.finish());
		Ok(())
	}

	/// Set the priority within the stream's send group, or session-wide when ungrouped.
	pub fn set_priority(&mut self, priority: i64) {
		match &mut self.state {
			State::Open(stream) => stream.set_priority(priority),
			State::Finished(stream) => stream.set_priority(priority),
			State::Reset(_) | State::Unsent => {}
		}
	}

	/// The stream's priority. See [`send::Stream::priority`].
	pub fn priority(&self) -> i64 {
		match &self.state {
			State::Open(stream) => stream.priority(),
			State::Finished(stream) => stream.priority(),
			State::Reset(_) | State::Unsent => 0,
		}
	}

	/// Move the stream into a send group, or with `None` back out. See [`send::Stream::set_group`].
	pub fn set_group(&mut self, group: Option<&send::Group>) {
		match &mut self.state {
			State::Open(stream) => stream.set_group(group),
			State::Finished(stream) => stream.set_group(group),
			State::Reset(_) | State::Unsent => {}
		}
	}

	/// Return the underlying QUIC stream ID.
	///
	/// > **Warning**
	/// >
	/// > WebTransport sessions share the QUIC connection with HTTP/3 and potentially other
	/// > sessions.
	/// > The [crate::quic::stream::Id::index] might not increment by 1 like expected when using raw QUIC.
	/// > This is why the Javascript WebTransport API does not expose the Stream ID.
	pub fn id(&self) -> crate::quic::stream::Id {
		self.id
	}
}

/// Decode the peer's stop code, and report a session error, or a stop for a session that is
/// gone, as the session's close reason, pending until that reason is known.
fn poll_error(close: &Handles, waiter: &kio::Waiter, e: send::Error) -> Poll<Error> {
	Poll::Ready(match e {
		send::Error::Stopped(code) => match close.decode_stream_code(code) {
			Some(code) => Error::Stopped(code),
			None => match close.poll_session_gone(waiter, code) {
				Some(err) => return err.map(Error::Session),
				None => Error::InvalidStopped(code),
			},
		},
		e => match Error::from(e) {
			Error::Session(e) => Error::Session(close.map(e)),
			e => e,
		},
	})
}

impl Drop for Stream {
	fn drop(&mut self) {
		// Chosen at drop, since the session may have closed since the stream opened.
		if let State::Open(stream) = &mut self.state {
			stream.set_drop_reset(self.header, self.close.encode_stream_code(0));
		}
	}
}

impl moq_net::transport::poll::SendStream for Stream {
	type Error = Error;
	type Group = send::Group;

	fn poll_write(&mut self, waiter: &kio::Waiter, mut buf: &[u8]) -> Poll<Result<usize, Self::Error>> {
		self.poll_write_buf(waiter, &mut buf)
	}
	fn poll_write_buf<B: Buf>(&mut self, waiter: &kio::Waiter, buf: &mut B) -> Poll<Result<usize, Self::Error>> {
		Self::poll_write_buf(self, waiter, buf)
	}
	fn set_priority(&mut self, priority: i64) {
		Self::set_priority(self, priority);
	}
	fn set_group(&mut self, group: &Self::Group) {
		Self::set_group(self, Some(group));
	}
	fn finish(&mut self) -> Result<(), Self::Error> {
		Self::finish_mut(self)
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
	fn acknowledged(&self) -> Option<bool> {
		if self.unwritten.has_remaining() {
			return Some(false);
		}
		Some(match &self.state {
			State::Open(stream) => stream.acknowledged(),
			State::Finished(stream) => stream.acknowledged(),
			State::Reset(_) | State::Unsent => false,
		})
	}

	/// The acknowledged bytes past the WebTransport header, none while the header is unwritten,
	/// and none once reset: a reset drops the handle that could tell.
	fn delivered(&self) -> Option<u64> {
		if self.unwritten.has_remaining() {
			return Some(0);
		}
		let acked = match &self.state {
			State::Open(stream) => stream.acked_len(),
			State::Finished(stream) => stream.acked_len(),
			State::Reset(_) | State::Unsent => 0,
		};
		Some(acked.saturating_sub(self.header.map_or(0, u64::from)))
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
	/// Resolves once the peer acknowledges or stops the stream (with its code), or the session closes.
	#[kio::wrapper]
	pub fn poll_closed(&self, waiter: &kio::Waiter) -> Poll<Result<Option<u32>, Error>> {
		self.stream.poll_closed(waiter)
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
	pub fn id(&self) -> crate::quic::stream::Id {
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

/// An error when writing to [`crate::quic::web::send::Stream`]. Similar to [`crate::quic::send::Error`].
#[derive(Clone, thiserror::Error, Debug)]
#[non_exhaustive]
pub enum Error {
	/// The peer stopped accepting this stream with its application code.
	#[error("STOP_SENDING: {0}")]
	Stopped(u32),

	/// The peer stopped the stream with a code outside the WebTransport registry.
	#[error("invalid STOP_SENDING: {0}")]
	InvalidStopped(crate::quic::VarInt),

	/// The session carrying the stream failed.
	#[error("session error: {0}")]
	Session(#[from] super::Error),

	/// The stream was already finished or reset.
	#[error("stream closed")]
	ClosedStream,

	/// The stream was finished before its WebTransport header fit the peer's flow control, so
	/// it was reset with nothing sent.
	#[error("finished before the stream header could be sent")]
	Unsent,
}

impl From<crate::quic::send::Error> for Error {
	fn from(e: crate::quic::send::Error) -> Self {
		match e {
			crate::quic::send::Error::Stopped(code) => match web_transport_proto::error_from_http3(code.into_inner()) {
				Some(code) => Error::Stopped(code),
				None => Error::InvalidStopped(code),
			},
			crate::quic::send::Error::Connection(e) => Error::Session(e.into()),
			// Only datagrams fail otherwise, and they report a session error.
			e => Error::Session(e.into()),
		}
	}
}

impl moq_net::transport::Error for Error {
	fn close_kind(&self) -> moq_net::transport::CloseKind {
		match self {
			Self::Session(e) => e.close_kind(),
			_ => moq_net::transport::CloseKind::Transport,
		}
	}
	fn session_error(&self) -> Option<(u32, String)> {
		if let Error::Session(e) = self {
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
pub use crate::quic::send::{Group, Options};
