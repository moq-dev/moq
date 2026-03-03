use std::fmt::Debug;

use crate::{Error, coding::*};

/// A wrapper around a [web_transport_trait::SendStream] that will reset on Drop.
pub struct Writer<S: web_transport_trait::SendStream, V> {
	stream: Option<S>,
	buffer: bytes::BytesMut,
	version: V,
}

impl<S: web_transport_trait::SendStream, V> Writer<S, V> {
	/// Create a new writer for the given stream and version.
	pub fn new(stream: S, version: V) -> Self {
		Self {
			stream: Some(stream),
			buffer: Default::default(),
			version,
		}
	}

	/// Encode the given message to the stream.
	pub async fn encode<T: Encode<V> + Debug>(&mut self, msg: &T) -> Result<(), Error>
	where
		V: Clone,
	{
		self.buffer.clear();
		msg.encode(&mut self.buffer, self.version.clone())?;

		while !self.buffer.is_empty() {
			self.stream
				.as_mut()
				.unwrap()
				.write_buf(&mut self.buffer)
				.await
				.map_err(Error::from_transport)?;
		}

		Ok(())
	}

	// Not public to avoid accidental partial writes.
	async fn write<Buf: bytes::Buf + Send>(&mut self, buf: &mut Buf) -> Result<usize, Error> {
		self.stream
			.as_mut()
			.unwrap()
			.write_buf(buf)
			.await
			.map_err(Error::from_transport)
	}

	/// Write the entire [Buf] to the stream.
	///
	/// NOTE: This can avoid performing a copy when using [Bytes].
	pub async fn write_all<Buf: bytes::Buf + Send>(&mut self, buf: &mut Buf) -> Result<(), Error> {
		while buf.has_remaining() {
			self.write(buf).await?;
		}
		Ok(())
	}

	/// Mark the stream as finished.
	pub fn finish(&mut self) -> Result<(), Error> {
		self.stream.as_mut().unwrap().finish().map_err(Error::from_transport)
	}

	/// Abort the stream with the given error.
	pub fn abort(&mut self, err: &Error) {
		self.stream.as_mut().unwrap().reset(err.to_code());
	}

	/// Wait for the stream to be closed, or the [Self::finish] to be acknowledged by the peer.
	pub async fn closed(&mut self) -> Result<(), Error> {
		self.stream
			.as_mut()
			.unwrap()
			.closed()
			.await
			.map_err(Error::from_transport)?;
		Ok(())
	}

	/// Set the priority of the stream.
	pub fn set_priority(&mut self, priority: u8) {
		self.stream.as_mut().unwrap().set_priority(priority);
	}

	/// Change the version, used after SETUP negotiation.
	pub fn set_version(&mut self, version: V) {
		self.version = version;
	}

	/// Convert to a writer with a different version type.
	pub fn map_version<V2>(self, version: V2) -> Writer<S, V2> {
		// SAFETY: We need to take the stream out without triggering Drop.
		// We use ManuallyDrop to prevent the old Writer from resetting the stream.
		let mut this = std::mem::ManuallyDrop::new(self);
		Writer::new(this.stream.take().unwrap(), version)
	}
}

impl<S: web_transport_trait::SendStream, V> Drop for Writer<S, V> {
	fn drop(&mut self) {
		if let Some(mut stream) = self.stream.take() {
			// Unlike the Quinn default, we abort the stream on drop.
			stream.reset(Error::Cancel.to_code());
		}
	}
}
