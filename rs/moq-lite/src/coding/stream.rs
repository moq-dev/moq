use crate::Error;
use crate::coding::{Reader, Writer};

/// A [Writer] and [Reader] pair for a single stream.
pub struct Stream<S: web_transport_trait::Session, V> {
	pub writer: Writer<S::SendStream, V>,
	pub reader: Reader<S::RecvStream, V>,
}

impl<S: web_transport_trait::Session, V> Stream<S, V> {
	/// Open a new stream with the given version.
	pub async fn open(session: &S, version: V) -> Result<Self, Error>
	where
		V: Clone,
	{
		let (send, recv) = session.open_bi().await.map_err(Error::from_transport)?;

		let writer = Writer::new(send, version.clone());
		let reader = Reader::new(recv, version);

		Ok(Stream { writer, reader })
	}

	/// Accept a new stream with the given version.
	pub async fn accept(session: &S, version: V) -> Result<Self, Error>
	where
		V: Clone,
	{
		let (send, recv) = session.accept_bi().await.map_err(Error::from_transport)?;

		let writer = Writer::new(send, version.clone());
		let reader = Reader::new(recv, version);

		Ok(Stream { writer, reader })
	}

	/// Cast the stream to a different version, used during version negotiation.
	pub fn with_version<O: Clone>(self, version: O) -> Stream<S, O> {
		Stream {
			writer: self.writer.with_version(version.clone()),
			reader: self.reader.with_version(version),
		}
	}
}
