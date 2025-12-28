use std::sync::Arc;

use crate::{coding::*, Error};

// A wrapper around a SendStream that will reset on Drop
pub struct Writer<S: web_transport_trait::SendStream, V> {
	stream: Option<S>,
	buffer: bytes::BytesMut,
	version: V,
}

impl<S: web_transport_trait::SendStream, V> Writer<S, V> {
	pub fn new(stream: S, version: V) -> Self {
		Self {
			stream: Some(stream),
			buffer: Default::default(),
			version,
		}
	}

	pub async fn encode<T: Encode<V>>(&mut self, msg: &T) -> Result<(), Error>
	where
		V: Clone,
	{
		self.buffer.clear();
		msg.encode(&mut self.buffer, self.version.clone());

		while !self.buffer.is_empty() {
			self.stream
				.as_mut()
				.unwrap()
				.write_buf(&mut self.buffer)
				.await
				.map_err(|e| Error::Transport(Arc::new(e)))?;
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
			.map_err(|e| Error::Transport(Arc::new(e)))
	}

	// NOTE: We use Buf so we don't perform a copy when using Quinn.
	pub async fn write_all<Buf: bytes::Buf + Send>(&mut self, buf: &mut Buf) -> Result<(), Error> {
		while buf.has_remaining() {
			self.write(buf).await?;
		}
		Ok(())
	}

	/// Mark the clean termination of the stream.
	pub fn finish(&mut self) -> Result<(), Error> {
		self.stream
			.as_mut()
			.unwrap()
			.finish()
			.map_err(|e| Error::Transport(Arc::new(e)))
	}

	pub fn abort(&mut self, err: &Error) {
		self.stream.as_mut().unwrap().reset(err.to_code());
	}

	pub async fn closed(&mut self) -> Result<(), Error> {
		self.stream
			.as_mut()
			.unwrap()
			.closed()
			.await
			.map_err(|e| Error::Transport(Arc::new(e)))?;
		Ok(())
	}

	pub fn set_priority(&mut self, priority: u8) {
		self.stream.as_mut().unwrap().set_priority(priority);
	}

	pub fn with_version<O>(mut self, version: O) -> Writer<S, O> {
		Writer {
			// We need to use an Option so Drop doesn't reset the stream.
			stream: self.stream.take(),
			buffer: std::mem::take(&mut self.buffer),
			version,
		}
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
