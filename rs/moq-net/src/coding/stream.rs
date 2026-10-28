use std::task::{Poll, ready};

use crate::Error;
use crate::coding::{Reader, StreamCodes, Writer};

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

impl<S: crate::transport::poll::Session, V: StreamCodes> Stream<S, V> {
	/// Poll opening a new stream with the given version, waiting for stream credit.
	pub fn poll_open(session: &S, version: V, waiter: &kio::Waiter) -> Poll<Result<Self, Error>>
	where
		V: Clone,
	{
		let (send, recv) =
			ready!(session.poll_open_bi(waiter, Default::default())).map_err(|err| version.transport_error(err))?;
		Poll::Ready(Ok(Self::build(send, recv, version)))
	}

	/// Open a new stream with the given version, waiting for stream credit.
	pub async fn open(session: &mut S, version: V) -> Result<Self, Error>
	where
		V: Clone,
	{
		kio::wait(|waiter| Self::poll_open(session, version.clone(), waiter)).await
	}

	/// Poll opening a request stream, refused with [Error::NoStreamCredit] when the peer
	/// grants no more streams instead of parking work the caller cannot see.
	pub fn poll_open_request(session: &S, version: V, waiter: &kio::Waiter) -> Poll<Result<Self, Error>>
	where
		V: Clone,
	{
		let (send, recv) = ready!(session.poll_open_request(waiter, Default::default()))
			.map_err(|err| version.transport_error(err))?
			.ok_or(Error::NoStreamCredit)?;
		Poll::Ready(Ok(Self::build(send, recv, version)))
	}

	/// Open a request stream, refused with [Error::NoStreamCredit] when the peer grants
	/// no more streams.
	pub async fn open_request(session: &mut S, version: V) -> Result<Self, Error>
	where
		V: Clone,
	{
		kio::wait(|waiter| Self::poll_open_request(session, version.clone(), waiter)).await
	}

	/// Poll accepting a new stream with the given version.
	pub fn poll_accept(session: &S, version: V, waiter: &kio::Waiter) -> Poll<Result<Self, Error>>
	where
		V: Clone,
	{
		let (send, recv) = ready!(session.poll_accept_bi(waiter)).map_err(|err| version.transport_error(err))?;
		Poll::Ready(Ok(Self::build(send, recv, version)))
	}

	/// Accept a new stream with the given version.
	pub async fn accept(session: &mut S, version: V) -> Result<Self, Error>
	where
		V: Clone,
	{
		let (send, recv) = session.accept_bi().await.map_err(|err| version.transport_error(err))?;
		Ok(Self::build(send, recv, version))
	}

	fn build(send: S::SendStream, recv: S::RecvStream, version: V) -> Self
	where
		V: Clone,
	{
		let mut writer = Writer::new(send, version.clone());
		writer.set_priority(CONTROL_SEND_ORDER.into());
		let mut reader = Reader::new(recv, version);
		reader.set_priority(crate::priority::RECV_CONTROL);
		Self { writer, reader }
	}

	pub(crate) fn with_stats(self, stats: &crate::stats::Session) -> Self {
		Self {
			writer: self.writer.with_stats(stats),
			reader: self.reader.with_stats(stats),
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

	#[test]
	fn request_refusal_is_local_and_does_not_open_a_stream() {
		use crate::lite::test_transport::ScriptedSession;
		for version in [
			crate::Version::Lite(Version::Lite07),
			crate::Version::Ietf(crate::ietf::Version::Draft18),
		] {
			let session = ScriptedSession::new(Vec::new()).without_credit();
			let waiter = &kio::Waiter::noop();
			for _ in 0..2 {
				assert!(matches!(
					Stream::poll_open_request(&session, version, waiter),
					Poll::Ready(Err(Error::NoStreamCredit))
				));
			}
			assert_eq!(session.log.bi_opens(), 0);
		}
		assert_eq!(
			crate::StreamError::from(&Error::NoStreamCredit),
			crate::StreamError::Internal
		);
		assert_eq!(
			crate::SessionError::from(&Error::NoStreamCredit),
			crate::SessionError::Internal
		);
	}

	#[test]
	fn request_header_backpressure_is_not_credit_refusal() {
		use crate::lite::test_transport::ScriptedSession;
		let gate = kio::Producer::new(false);
		let session = ScriptedSession::gated_open(Vec::new(), gate.consume());
		let waiter = &kio::Waiter::noop();
		assert!(Stream::poll_open_request(&session, Version::Lite07, waiter).is_pending());
		{
			let Ok(mut open) = gate.write() else {
				panic!("gate is still live");
			};
			*open = true;
		}
		assert!(matches!(
			Stream::poll_open_request(&session, Version::Lite07, waiter),
			Poll::Ready(Ok(_))
		));
		assert_eq!(session.log.bi_opens(), 1);
	}

	/// Opening a control stream must set its send order, or a saturated link
	/// starves it: quinn schedules strictly by priority and round-robins only
	/// within a level, so a rank-0 group with bytes pending is enough to keep
	/// the scheduler from ever reaching the transport's default of 0.
	#[moq_clock::test]
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
	#[moq_clock::test]
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
