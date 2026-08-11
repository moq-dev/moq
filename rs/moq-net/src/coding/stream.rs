use std::task::{Context, Poll, ready};

use crate::Error;
use crate::coding::{Reader, Writer};

/// A [Writer] and [Reader] pair for a single stream.
pub struct Stream<S: crate::transport::poll::Session, V> {
	pub writer: Writer<S::SendStream, V>,
	pub reader: Reader<S::RecvStream, V>,
}

impl<S: crate::transport::poll::Session, V> Stream<S, V> {
	/// Poll opening a new stream with the given version.
	pub fn poll_open(session: &mut S, version: V, cx: &mut Context<'_>) -> Poll<Result<Self, Error>>
	where
		V: Clone,
	{
		let (send, recv) = ready!(session.poll_open_bi(cx)).map_err(Error::from_transport)?;
		Poll::Ready(Ok(Self::build(send, recv, version)))
	}

	/// Open a new stream with the given version.
	pub async fn open(session: &mut S, version: V) -> Result<Self, Error>
	where
		V: Clone,
	{
		let (send, recv) = session.open_bi().await.map_err(Error::from_transport)?;
		Ok(Self::build(send, recv, version))
	}

	/// Poll accepting a new stream with the given version.
	pub fn poll_accept(session: &mut S, version: V, cx: &mut Context<'_>) -> Poll<Result<Self, Error>>
	where
		V: Clone,
	{
		let (send, recv) = ready!(session.poll_accept_bi(cx)).map_err(Error::from_transport)?;
		Poll::Ready(Ok(Self::build(send, recv, version)))
	}

	/// Accept a new stream with the given version.
	pub async fn accept(session: &mut S, version: V) -> Result<Self, Error>
	where
		V: Clone,
	{
		let (send, recv) = session.accept_bi().await.map_err(Error::from_transport)?;
		Ok(Self::build(send, recv, version))
	}

	fn build(send: S::SendStream, recv: S::RecvStream, version: V) -> Self
	where
		V: Clone,
	{
		Self {
			writer: Writer::new(send, version.clone()),
			reader: Reader::new(recv, version),
		}
	}

	/// Cast the stream to a different version, used during version negotiation.
	pub fn with_version<V2: Clone>(self, version: V2) -> Stream<S, V2> {
		Stream {
			writer: self.writer.with_version(version.clone()),
			reader: self.reader.with_version(version),
		}
	}
}
