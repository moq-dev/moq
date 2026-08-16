use std::{
	cmp,
	fmt::Debug,
	io,
	task::{Context, Poll, ready},
};

use bytes::{Buf, Bytes, BytesMut};

use crate::{Error, StreamError, coding::*};

/// A reader for decoding messages from a stream.
///
/// The `poll_*` methods are the implementation; the `async` methods are thin
/// wrappers, so the reader can be driven from another poll function without
/// pinning a future. Partial bytes accumulate in the internal buffer, so a
/// `Pending` (or a cancelled wrapper) never desynchronizes the stream.
pub struct Reader<S: crate::transport::poll::RecvStream, V> {
	stream: S,
	buffer: BytesMut,
	version: V,
}

impl<S: crate::transport::poll::RecvStream, V> Reader<S, V> {
	pub fn new(stream: S, version: V) -> Self {
		Self {
			stream,
			buffer: Default::default(),
			version,
		}
	}

	/// Poll for the next message on the stream.
	pub fn poll_decode<T: Decode<V> + Debug>(&mut self, cx: &mut Context<'_>) -> Poll<Result<T, Error>>
	where
		V: Clone,
	{
		loop {
			let mut cursor = io::Cursor::new(&self.buffer);
			match T::decode(&mut cursor, self.version.clone()) {
				Ok(msg) => {
					self.buffer.advance(cursor.position() as usize);
					return Poll::Ready(Ok(msg));
				}
				// Stream closed while we still need more data.
				Err(DecodeError::Short) if !ready!(self.poll_read_more(cx))? => {
					return Poll::Ready(Err(DecodeError::Short.into()));
				}
				Err(DecodeError::Short) => {}
				Err(e) => return Poll::Ready(Err(e.into())),
			}
		}
	}

	/// Decode the next message from the stream.
	pub async fn decode<T: Decode<V> + Debug>(&mut self) -> Result<T, Error>
	where
		V: Clone,
	{
		std::future::poll_fn(|cx| self.poll_decode(cx)).await
	}

	/// Poll for the next message unless the stream is closed cleanly first.
	pub fn poll_decode_maybe<T: Decode<V> + Debug>(&mut self, cx: &mut Context<'_>) -> Poll<Result<Option<T>, Error>>
	where
		V: Clone,
	{
		if !ready!(self.poll_has_more(cx))? {
			return Poll::Ready(Ok(None));
		}

		self.poll_decode(cx).map_ok(Some)
	}

	/// Decode the next message unless the stream is closed.
	pub async fn decode_maybe<T: Decode<V> + Debug>(&mut self) -> Result<Option<T>, Error>
	where
		V: Clone,
	{
		std::future::poll_fn(|cx| self.poll_decode_maybe(cx)).await
	}

	/// Poll for the next message without consuming it.
	pub fn poll_decode_peek<T: Decode<V> + Debug>(&mut self, cx: &mut Context<'_>) -> Poll<Result<T, Error>>
	where
		V: Clone,
	{
		loop {
			let mut cursor = io::Cursor::new(&self.buffer);
			match T::decode(&mut cursor, self.version.clone()) {
				Ok(msg) => return Poll::Ready(Ok(msg)),
				Err(DecodeError::Short) if !ready!(self.poll_read_more(cx))? => {
					return Poll::Ready(Err(DecodeError::Short.into()));
				}
				Err(DecodeError::Short) => {}
				Err(e) => return Poll::Ready(Err(e.into())),
			}
		}
	}

	/// Decode the next message from the stream without consuming it.
	pub async fn decode_peek<T: Decode<V> + Debug>(&mut self) -> Result<T, Error>
	where
		V: Clone,
	{
		std::future::poll_fn(|cx| self.poll_decode_peek(cx)).await
	}

	/// Poll for the next chunk, draining the reader's internal buffer first.
	pub fn poll_read_chunk(&mut self, cx: &mut Context<'_>, max: usize) -> Poll<Result<Option<Bytes>, Error>> {
		if !self.buffer.is_empty() {
			let n = cmp::min(self.buffer.len(), max);
			return Poll::Ready(Ok(Some(self.buffer.split_to(n).freeze())));
		}
		self.stream.poll_read_chunk(cx, max).map_err(Error::from_transport)
	}

	/// Poll for exactly `size` bytes, accumulating partial reads in the buffer.
	pub fn poll_read_exact(&mut self, cx: &mut Context<'_>, size: usize) -> Poll<Result<Bytes, Error>> {
		while self.buffer.len() < size {
			if !ready!(self.poll_read_more(cx))? {
				return Poll::Ready(Err(DecodeError::Short.into()));
			}
		}
		Poll::Ready(Ok(self.buffer.split_to(size).freeze()))
	}

	/// Read exactly the given number of bytes from the stream.
	pub async fn read_exact(&mut self, size: usize) -> Result<Bytes, Error> {
		std::future::poll_fn(|cx| self.poll_read_exact(cx, size)).await
	}

	/// Poll until the stream is closed, erroring if there are any additional bytes.
	pub fn poll_closed(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Error>> {
		if ready!(self.poll_has_more(cx))? {
			return Poll::Ready(Err(DecodeError::Short.into()));
		}

		Poll::Ready(Ok(()))
	}

	/// Poll for whether data is available in the buffer or stream.
	fn poll_has_more(&mut self, cx: &mut Context<'_>) -> Poll<Result<bool, Error>> {
		if !self.buffer.is_empty() {
			return Poll::Ready(Ok(true));
		}

		self.poll_read_more(cx)
	}

	/// Poll for more data from the stream. `true` if data was read, `false` if the
	/// stream finished.
	fn poll_read_more(&mut self, cx: &mut Context<'_>) -> Poll<Result<bool, Error>> {
		match ready!(self.stream.poll_read_buf(cx, &mut self.buffer)) {
			Ok(Some(_)) => Poll::Ready(Ok(true)),
			Ok(None) => Poll::Ready(Ok(false)),
			Err(e) => Poll::Ready(Err(Error::from_transport(e))),
		}
	}

	/// Abort the stream with the given error.
	pub fn abort(&mut self, err: &Error) {
		// STOP_SENDING is a stream operation, so it carries a stream code. Sending the
		// session code here would have the peer read it against the wrong registry.
		self.stream.stop(StreamError::from(err).to_code());
	}

	/// Cast the reader to a different version, used during version negotiation.
	pub fn with_version<V2>(self, version: V2) -> Reader<S, V2> {
		Reader {
			stream: self.stream,
			buffer: self.buffer,
			version,
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::StreamError;

	/// Records the STOP_SENDING code so a test can assert which registry it came from.
	#[derive(Default)]
	struct StopLog {
		stops: Vec<u32>,
	}

	impl web_transport_trait::poll::RecvStream for StopLog {
		type Error = crate::lite::test_transport::SinkError;

		fn poll_read(
			&mut self,
			_cx: &mut std::task::Context<'_>,
			_dst: &mut [u8],
		) -> std::task::Poll<Result<Option<usize>, Self::Error>> {
			std::task::Poll::Ready(Ok(None))
		}

		fn stop(&mut self, code: u32) {
			self.stops.push(code);
		}

		fn poll_closed(&mut self, _cx: &mut std::task::Context<'_>) -> std::task::Poll<Result<(), Self::Error>> {
			std::task::Poll::Ready(Ok(()))
		}
	}

	/// STOP_SENDING refuses a stream, so it must carry a stream code. Reusing the session
	/// table here would have the peer decode it against the wrong registry: `Cancel` would
	/// arrive as INTERNAL_ERROR, and `Unauthorized` as KEY_VALUE_FORMATTING_ERROR.
	#[test]
	fn abort_stops_with_a_stream_code() {
		for (err, expected) in [
			(Error::Cancel, StreamError::Cancel.to_code()),
			(Error::Lagged, StreamError::TooFarBehind.to_code()),
			(
				Error::Unauthorized,
				StreamError::Session(crate::SessionError::Unauthorized).to_code(),
			),
		] {
			let mut reader = Reader::new(StopLog::default(), ());
			reader.abort(&err);
			assert_eq!(reader.stream.stops, vec![expected], "{err:?} used the wrong registry");
		}

		// The two registries disagree about 0 and 1, which is what makes the mix-up silent.
		assert_ne!(StreamError::Cancel.to_code(), crate::SessionError::Cancel.to_code());
	}
}
