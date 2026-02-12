use std::{cmp, fmt::Debug, io};

use bytes::{Buf, BufMut, Bytes, BytesMut};

use crate::{Error, coding::*};

/// A reader for decoding messages from a stream.
pub struct Reader<S: web_transport_trait::RecvStream, V> {
	stream: S,
	buffer: BytesMut,
	version: V,
}

impl<S: web_transport_trait::RecvStream, V> Reader<S, V> {
	pub fn new(stream: S, version: V) -> Self {
		Self {
			stream,
			buffer: Default::default(),
			version,
		}
	}

	/// Decode the next message from the stream.
	pub async fn decode<T: Decode<V> + Debug>(&mut self) -> Result<T, Error>
	where
		V: Clone,
	{
		loop {
			let mut cursor = io::Cursor::new(&self.buffer);
			match T::decode(&mut cursor, self.version.clone()) {
				Ok(msg) => {
					self.buffer.advance(cursor.position() as usize);
					return Ok(msg);
				}
				Err(DecodeError::Short) => {
					// Try to read more data
					if !self.read_more().await? {
						// Stream closed while we still need more data
						return Err(Error::Decode);
					}
				}
				Err(e) => return Err(e.into()),
			}
		}
	}

	/// Decode the next message unless the stream is closed.
	pub async fn decode_maybe<T: Decode<V> + Debug>(&mut self) -> Result<Option<T>, Error>
	where
		V: Clone,
	{
		if !self.has_more().await? {
			return Ok(None);
		}

		Ok(Some(self.decode().await?))
	}

	/// Decode the next message from the stream without consuming it.
	pub async fn decode_peek<T: Decode<V> + Debug>(&mut self) -> Result<T, Error>
	where
		V: Clone,
	{
		loop {
			let mut cursor = io::Cursor::new(&self.buffer);
			match T::decode(&mut cursor, self.version.clone()) {
				Ok(msg) => return Ok(msg),
				Err(DecodeError::Short) => {
					// Try to read more data
					if !self.read_more().await? {
						// Stream closed while we still need more data
						return Err(Error::Decode);
					}
				}
				Err(e) => return Err(e.into()),
			}
		}
	}

	/// Returns a non-zero chunk of data, or None if the stream is closed
	pub async fn read(&mut self, max: usize) -> Result<Option<Bytes>, Error> {
		if !self.buffer.is_empty() {
			let size = cmp::min(max, self.buffer.len());
			let data = self.buffer.split_to(size).freeze();
			return Ok(Some(data));
		}

		self.stream.read_chunk(max).await.map_err(Error::from_transport)
	}

	/// Read exactly the given number of bytes from the stream.
	pub async fn read_exact(&mut self, size: usize) -> Result<Bytes, Error> {
		// An optimization to avoid a copy if we have enough data in the buffer
		if self.buffer.len() >= size {
			return Ok(self.buffer.split_to(size).freeze());
		}

		let data = BytesMut::with_capacity(size.min(u16::MAX as usize));
		let mut buf = data.limit(size);

		let size = cmp::min(buf.remaining_mut(), self.buffer.len());
		let data = self.buffer.split_to(size);
		buf.put(data);

		while buf.has_remaining_mut() {
			match self.stream.read_buf(&mut buf).await {
				Ok(Some(_)) => {}
				Ok(None) => return Err(Error::Decode),
				Err(e) => return Err(Error::from_transport(e)),
			}
		}

		Ok(buf.into_inner().freeze())
	}

	/// Skip the given number of bytes from the stream.
	pub async fn skip(&mut self, mut size: usize) -> Result<(), Error> {
		let buffered = self.buffer.len().min(size);
		self.buffer.advance(buffered);
		size -= buffered;

		while size > 0 {
			let chunk = self
				.stream
				.read_chunk(size)
				.await
				.map_err(Error::from_transport)?
				.ok_or(Error::Decode)?;
			size -= chunk.len();
		}

		Ok(())
	}

	/// Wait until the stream is closed, erroring if there are any additional bytes.
	pub async fn closed(&mut self) -> Result<(), Error> {
		if self.has_more().await? {
			return Err(Error::Decode);
		}

		Ok(())
	}

	/// Returns true if there is more data available in the buffer or stream.
	async fn has_more(&mut self) -> Result<bool, Error> {
		if !self.buffer.is_empty() {
			return Ok(true);
		}

		self.read_more().await
	}

	/// Try to read more data from the stream. Returns true if data was read, false if stream closed.
	async fn read_more(&mut self) -> Result<bool, Error> {
		match self.stream.read_buf(&mut self.buffer).await {
			Ok(Some(_)) => Ok(true),
			Ok(None) => Ok(false),
			Err(e) => Err(Error::from_transport(e)),
		}
	}

	/// Abort the stream with the given error.
	pub fn abort(&mut self, err: &Error) {
		self.stream.stop(err.to_code());
	}

	/// Cast the reader to a different version, used during version negotiation.
	pub fn with_version<O>(self, version: O) -> Reader<S, O> {
		Reader {
			stream: self.stream,
			buffer: self.buffer,
			version,
		}
	}
}
