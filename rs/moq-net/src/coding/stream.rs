use std::task::{Context, Poll, ready};

use crate::Error;
use crate::coding::{Reader, Writer};

/// The send order every control stream is opened at.
///
/// Control streams carry announces, subscribe responses and track info: tens of
/// bytes each, and nothing downstream can proceed until they arrive. Left at the
/// transport's default of 0 they lose to every media stream, because a group
/// stream's send order is `u8::MAX - rank` and quinn schedules strictly by
/// priority, round-robining only within one level. On a link saturated by the
/// media itself the top-ranked group always has bytes pending, so the scheduler
/// never reaches 0 and a subscriber cannot learn anything new from its publisher
/// for as long as the saturation lasts.
///
/// `u8::MAX` ties with the most urgent group rather than preempting it, so
/// relative media ordering is untouched and a control message takes a
/// round-robin share: a packet or two, which is all it needs.
const CONTROL_SEND_ORDER: u8 = u8::MAX;

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
		let mut writer = Writer::new(send, version.clone());
		writer.set_priority(CONTROL_SEND_ORDER);
		Self {
			writer,
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

#[cfg(test)]
mod tests {
	use super::*;
	use crate::lite::{Version, test_transport::SinkSession};

	/// Opening a control stream must set its send order, or a saturated link
	/// starves it: quinn schedules strictly by priority and round-robins only
	/// within a level, so a rank-0 group with bytes pending is enough to keep
	/// the scheduler from ever reaching the transport's default of 0.
	#[tokio::test]
	async fn open_prioritises_the_stream() {
		let gate = kio::Producer::new(true);
		let mut session = SinkSession::gated_bi(gate.consume());
		let log = session.log.clone();

		let _stream = Stream::open(&mut session, Version::Lite05).await.unwrap();

		assert_eq!(
			log.priorities(),
			vec![CONTROL_SEND_ORDER],
			"a control stream left at the transport default loses to every group",
		);
	}

	/// The accepted half answers on the stream it was handed (track info, subscribe
	/// responses), so it needs the same order as one we opened. A fix that only
	/// covered `open` would leave every reply behind the media.
	#[tokio::test]
	async fn accept_prioritises_the_stream() {
		let gate = kio::Producer::new(true);
		let mut session = SinkSession::accepted_bi(gate.consume());
		let log = session.log.clone();

		let _stream = Stream::accept(&mut session, Version::Lite05).await.unwrap();

		assert_eq!(
			log.priorities(),
			vec![CONTROL_SEND_ORDER],
			"an accepted control stream left at the transport default loses to every group",
		);
	}
}
