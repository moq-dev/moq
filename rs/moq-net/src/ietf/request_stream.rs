use crate::driver::Transport;
use crate::transport::poll::Session as _;
use std::task::{Poll, ready};

use super::{Message, Version};
use crate::{
	Error,
	coding::{Decode, DecodeError, Decoder, Stream},
};

pub(super) fn fin_cancels(version: Version) -> bool {
	matches!(version, Version::Draft18)
}

/// Watch abrupt cancellation after the requester has finished sending messages.
pub(super) fn poll_cancel(
	stream: &mut Stream<Transport, Version>,
	finished: &mut bool,
	messages: &mut Messages,
	version: Version,
	waiter: &kio::Waiter,
) -> Poll<Result<(), Error>> {
	if !*finished {
		match messages.poll_end(&mut stream.reader, waiter) {
			Poll::Ready(Ok(())) if !fin_cancels(version) => *finished = true,
			Poll::Ready(result) => return Poll::Ready(result),
			Poll::Pending => {}
		}
	}
	stream.writer.poll_closed(waiter)
}

/// Watch for the requester ending a request it has not been answered on yet, without consuming
/// what it sent meanwhile: a waiting message (an update, say) ends the watch on the reader and
/// is read once the request is answered. A withdrawal queued behind an update is therefore seen
/// only then; until the verifier answers, the requester holds nothing but its own slot.
pub(super) fn poll_withdrawn(
	stream: &mut Stream<Transport, Version>,
	version: Version,
	waiter: &kio::Waiter,
) -> Poll<()> {
	if stream.writer.poll_closed(waiter).is_ready() {
		return Poll::Ready(());
	}
	match stream.reader.poll_has_more(waiter) {
		Poll::Ready(Ok(false)) if fin_cancels(version) => Poll::Ready(()),
		Poll::Ready(Err(_)) => Poll::Ready(()),
		_ => Poll::Pending,
	}
}

/// One whole `[type][length][body]` message off a request stream, decoded at once so a read
/// dropped part-way consumes nothing.
#[derive(Debug)]
pub(super) struct Framed(pub(super) u64, pub(super) super::Body);

impl<'a> Decode<'a, Version> for Framed {
	fn decode(r: &mut Decoder<'a>, version: Version) -> Result<Self, DecodeError> {
		let id = r.varint()?;
		let start = r.position();
		let body = super::Body::decode(r, version)?;
		r.control = Some(super::message::control(id));
		r.uncounted = start;
		Ok(Self(id, body))
	}
}

/// A message following the request on a request stream: `T`, or a GOAWAY migrating this
/// one request (draft-22 section 9.2).
#[derive(Debug)]
pub(super) enum Next<T> {
	Message(T),
	/// `redirect` is whether it names a new session, which only a server may do.
	GoAway {
		redirect: bool,
	},
}

impl<'a, T: Decode<'a, Version>> Decode<'a, Version> for Next<T> {
	fn decode(r: &mut Decoder<'a>, version: Version) -> Result<Self, DecodeError> {
		let mut peek = r.clone();
		if peek.varint()? == super::GoAway::ID {
			*r = peek;
			r.uncounted = r.position();
			let msg = super::GoAway::decode(r, version)?;
			// Draft-18's Request ID belongs on the control stream only.
			if msg.request_id.is_some() {
				return Err(DecodeError::InvalidValue);
			}
			return Ok(Self::GoAway {
				redirect: !msg.new_session_uri.is_empty(),
			});
		}
		T::decode(r, version).map(Self::Message)
	}
}

/// The GOAWAYs the peer sent on one request stream (draft-22 section 9.2).
pub(super) struct GoAways {
	/// Our own role: a client's peer is a server, which may name a new session.
	client: bool,
	seen: bool,
}

impl GoAways {
	pub fn new(client: bool) -> Self {
		Self { client, seen: false }
	}

	/// Note a GOAWAY: the first is legal, but a second one, or a client naming a new
	/// session, closes the session.
	pub fn receive(&mut self, redirect: bool) -> Result<(), Error> {
		if std::mem::replace(&mut self.seen, true) || (redirect && !self.client) {
			return Err(Error::ProtocolViolation);
		}
		Ok(())
	}

	/// Read the next `T`, consuming at most one GOAWAY before it.
	pub fn poll_next<T: for<'a> Decode<'a, Version> + std::fmt::Debug, R: crate::transport::poll::RecvStream>(
		&mut self,
		reader: &mut crate::coding::Reader<R, Version>,
		waiter: &kio::Waiter,
	) -> Poll<Result<Option<T>, Error>> {
		loop {
			match ready!(reader.poll_decode_maybe::<Next<T>>(waiter))? {
				Some(Next::GoAway { redirect }) => self.receive(redirect)?,
				Some(Next::Message(message)) => return Poll::Ready(Ok(Some(message))),
				None => return Poll::Ready(Ok(None)),
			}
		}
	}
}

/// The messages on one request stream, keeping GOAWAY validation across response phases.
pub(super) struct Messages {
	session: Transport,
	goaways: GoAways,
}

impl Messages {
	pub fn new(session: Transport, client: bool) -> Self {
		Self {
			session,
			goaways: GoAways::new(client),
		}
	}

	pub fn failed(&mut self, err: Error) -> Error {
		if super::subscriber::is_protocol_violation(&err) {
			self.session.close(
				super::error::to_session_code(&crate::SessionError::from(&err)),
				&err.to_string(),
			);
		}
		err
	}

	pub fn receive(&mut self, redirect: bool) -> Result<(), Error> {
		self.goaways.receive(redirect).map_err(|err| self.failed(err))
	}

	/// Read complete frames, consuming at most one GOAWAY without ending the request.
	pub fn poll_next<R: crate::transport::poll::RecvStream>(
		&mut self,
		reader: &mut crate::coding::Reader<R, Version>,
		waiter: &kio::Waiter,
	) -> Poll<Result<Option<Framed>, Error>> {
		self.goaways.poll_next(reader, waiter).map_err(|err| self.failed(err))
	}

	pub async fn next<R: crate::transport::poll::RecvStream>(
		&mut self,
		reader: &mut crate::coding::Reader<R, Version>,
	) -> Result<Option<Framed>, Error> {
		kio::wait(|waiter| self.poll_next(reader, waiter)).await
	}

	pub async fn message<R: crate::transport::poll::RecvStream>(
		&mut self,
		reader: &mut crate::coding::Reader<R, Version>,
	) -> Result<Framed, Error> {
		self.next(reader)
			.await?
			.ok_or_else(|| self.failed(Error::ProtocolViolation))
	}

	pub fn poll_end<R: crate::transport::poll::RecvStream>(
		&mut self,
		reader: &mut crate::coding::Reader<R, Version>,
		waiter: &kio::Waiter,
	) -> Poll<Result<(), Error>> {
		match ready!(self.poll_next(reader, waiter))? {
			None => Poll::Ready(Ok(())),
			Some(_) => Poll::Ready(Err(self.failed(Error::UnexpectedMessage))),
		}
	}
}

/// Refuse a request with its complete REQUEST_ERROR, then finish the response.
pub(super) async fn reject(
	mut stream: Stream<Transport, Version>,
	error: super::RequestError<'_>,
) -> Result<(), Error> {
	stream.writer.encode_message(&error).await?;
	// A peer dropping the response after reading the refusal is a normal end.
	let _ = stream.writer.close().await;
	Ok(())
}

/// Write the REQUEST_ERROR refusing a `kind` request with `err`.
pub(super) async fn write_error<W: crate::transport::poll::SendStream>(
	writer: &mut crate::coding::Writer<W, Version>,
	kind: super::error::request::Kind,
	err: &Error,
	reason: &str,
	version: Version,
) -> Result<(), Error> {
	writer.buffer_message(
		super::RequestError::ID,
		&super::RequestError {
			error_code: super::error::request::to_code(err, kind, version),
			reason_phrase: reason.into(),
			retry_interval: 0,
		},
	)?;
	kio::wait(|waiter| writer.poll_flush(waiter)).await
}

/// A framed subscription update, preserving omitted preferences.
#[derive(Debug)]
pub(super) struct Update {
	/// The update's own Request ID.
	pub(super) request_id: super::RequestId,
	pub(super) priority: Option<u8>,
	pub(super) max_age: Option<u64>,
	/// Why the update is refused, which fails the subscription: [`Error::GoingAway`],
	/// [`Error::Unsupported`], a
	/// [`DecodeError::MalformedAuthToken`] for tokens that repeat, or a
	/// [`DecodeError::MultipleAuthTokens`] for more than one token.
	pub(super) refused: Option<Error>,
	/// The AUTHORIZATION TOKEN renewing the request's credential.
	pub(super) authorization_token: Option<super::token::RequestToken>,
	/// NEW_GROUP_REQUEST, which only a track advertising DYNAMIC_GROUPS takes.
	pub(super) new_group: Option<u64>,
	/// FORWARD, `None` when unchanged.
	pub(super) forward: Option<bool>,
	/// The new Location Filter replacing the subscription's requested range.
	pub(super) filter: Option<super::Filter>,
	/// FILL_PARAMETERS (draft-22): the backfill requested with this update.
	pub(super) fill: Option<super::Fill>,
}

impl Update {
	/// Decode the message body, after its type and length.
	fn decode_body(data: &mut Decoder<'_>, version: Version) -> Result<Self, DecodeError> {
		use super::{Fill, Filter, Opaque, RequestId};
		let request_id = RequestId::decode(data, version)?;
		decode_params!(data, version,
			0x02 => object_timeout: Option<u64>,
			0x03 => token: Vec<super::token::RequestToken>,
			0x06 => subgroup_timeout: Option<u64>,
			0x10 => forward: Option<bool>,
			0x20 => priority: Option<u8>,
			0x21 => filter: Option<Filter>,
			0x23 => fill: Option<Fill> where Filter::is_draft22(version),
			0x25 => subgroup_filter: Vec<Opaque> where super::subscribe::has_range_filters(version),
			0x26 => object_filter: Vec<Opaque> where super::subscribe::has_range_filters(version),
			0x27 => priority_filter: Vec<Opaque> where super::subscribe::has_range_filters(version),
			0x28 => property_filter: Vec<Opaque> where super::subscribe::has_range_filters(version),
			0x29 => track_filter: Vec<Opaque> where super::subscribe::has_range_filters(version),
			0x32 => new_group: Option<u64>,
			super::max_age::PARAMETER => max_age: Option<u64>,
		);
		if !data.is_empty() {
			return Err(DecodeError::InvalidValue);
		}
		// TRACK_PROPERTY_FILTER belongs only to SUBSCRIBE_TRACKS and its updates.
		if !track_filter.is_empty() {
			return Err(DecodeError::InvalidValue);
		}
		let unsupported = !subgroup_filter.is_empty()
			|| !object_filter.is_empty()
			|| !priority_filter.is_empty()
			|| !property_filter.is_empty();
		let (refused, authorization_token) = Self::refused(token, unsupported);
		Ok(Self {
			request_id,
			priority,
			max_age: super::max_age::validate(max_age)?,
			refused,
			authorization_token,
			new_group,
			forward,
			filter,
			fill,
		})
	}

	/// Why an update is refused, if it is, and the token it carries. A token refusal comes
	/// first: section 8.9 rejects an update whose tokens repeat, which says more than that we
	/// do not support it.
	fn refused(
		tokens: Vec<super::token::RequestToken>,
		unsupported: bool,
	) -> (Option<Error>, Option<super::token::RequestToken>) {
		match super::token::single(tokens) {
			Err(err) => (Some(err.into()), None),
			Ok(token) => (unsupported.then_some(Error::Unsupported), token),
		}
	}
}

impl<'a> Decode<'a, Version> for Update {
	fn decode(r: &mut Decoder<'a>, version: Version) -> Result<Self, DecodeError> {
		if r.varint()? != super::SubscribeUpdate::ID {
			return Err(DecodeError::InvalidValue);
		}
		let start = r.position();
		let size = r.u16()? as usize;
		let mut data = r.sub(size)?;
		// The complete frame is present; a short field inside it is malformed.
		let update = Self::decode_body(&mut data, version).map_err(DecodeError::complete)?;
		r.control = Some(super::message::control(super::SubscribeUpdate::ID));
		r.uncounted = start;
		Ok(update)
	}
}

/// The namespace-only parameters of a REQUEST_UPDATE. `TRACKS` marks one on a
/// SUBSCRIBE_TRACKS, which from draft-22 may also carry TRACK_PROPERTY_FILTER and
/// FORWARD.
#[derive(Debug)]
pub(super) struct NamespaceUpdate<const TRACKS: bool = false> {
	pub(super) request_id: super::RequestId,
	pub(super) prefix: Option<crate::Path>,
	pub(super) refused: Option<Error>,
}

#[derive(Debug)]
struct NamespacePrefix(crate::Path);

impl super::parameters::Param for NamespacePrefix {
	fn param_encode(&self, w: &mut crate::coding::Encoder<'_>, _: Version) -> Result<(), crate::coding::EncodeError> {
		super::namespace::encode_namespace(w, &self.0)
	}
	fn param_decode(r: &mut Decoder<'_>, _: Version) -> Result<Self, DecodeError> {
		Ok(Self(super::namespace::decode_namespace(r)?))
	}
}

impl<'a, const TRACKS: bool> Decode<'a, Version> for NamespaceUpdate<TRACKS> {
	fn decode(r: &mut Decoder<'a>, version: Version) -> Result<Self, DecodeError> {
		if r.varint()? != super::SubscribeUpdate::ID {
			return Err(DecodeError::InvalidValue);
		}
		let start = r.position();
		let size = r.u16()? as usize;
		let mut data = r.sub(size)?;
		let mut decode = || {
			let request_id = super::RequestId::decode(&mut data, version)?;
			decode_params!(&mut data, version,
				0x03 => token: Vec<super::token::RequestToken>,
				0x10 => _forward: Option<bool> where TRACKS && super::Filter::is_draft22(version),
				0x29 => _track_filter: Vec<super::Opaque> where TRACKS && super::subscribe::has_range_filters(version),
				0x34 => prefix: Option<NamespacePrefix>,
			);
			if !data.is_empty() {
				return Err(DecodeError::InvalidValue);
			}
			Ok(Self {
				request_id,
				prefix: prefix.map(|p| p.0),
				refused: super::token::single(token).err().map(Into::into),
			})
		};
		let update = decode().map_err(DecodeError::complete)?;
		r.control = Some(super::message::control(super::SubscribeUpdate::ID));
		r.uncounted = start;
		Ok(update)
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[moq_clock::test]
	async fn request_messages_keep_goaway_state_across_response_phases() {
		use crate::lite::test_transport::ScriptedSession;
		for version in [Version::Draft18, Version::Draft22] {
			// GOAWAY, REQUEST_OK, then another GOAWAY on the same stream.
			let script = vec![0x10, 0, 2, 0, 0, 0x07, 0, 0, 0x10, 0, 2, 0, 0];
			let session = ScriptedSession::eof(script);
			let mut stream = Stream::open(&mut crate::driver::Transport::new(session.clone()), version)
				.await
				.unwrap();
			let mut messages = Messages::new(crate::driver::Transport::new(session.clone()), true);
			assert_eq!(messages.message(&mut stream.reader).await.unwrap().0, 0x07);
			assert!(matches!(
				messages.message(&mut stream.reader).await,
				Err(Error::ProtocolViolation)
			));
			assert_eq!(session.log.closes()[0].0, 0x03, "{version}");
		}
	}

	/// A request-stream message counts as its sender counts it: the length and body, not the
	/// type that selects it, including a GOAWAY read ahead of the response.
	#[moq_clock::test]
	async fn request_stream_messages_count_without_their_type() {
		use crate::lite::test_transport::ScriptedSession;
		use crate::stats::{Message, Registry, Tier};
		let registry = Registry::new(Default::default());
		let stats = registry.tier(Tier::default()).session(crate::Path::from_static(""));
		let version = Version::Draft22;
		let session = ScriptedSession::new(vec![0x10, 0, 2, 0, 0, 0x07, 0, 0]);
		let mut stream = Stream::open(&mut crate::driver::Transport::new(session.clone()), version)
			.await
			.unwrap()
			.with_stats(&stats);
		let mut messages = Messages::new(crate::driver::Transport::new(session.clone()), true);
		assert_eq!(messages.message(&mut stream.reader).await.unwrap().0, 0x07);
		let rx = |class| {
			let rows = registry.snapshot().control();
			rows.into_iter().find(|row| row.1 == class).map(|row| row.2.rx)
		};
		assert_eq!(rx(Message::Goaway), Some(4));
		assert_eq!(rx(Message::Other), Some(2));
	}

	#[moq_clock::test]
	async fn a_goaway_does_not_cancel_a_pending_request() {
		use crate::lite::test_transport::ScriptedSession;
		for version in [Version::Draft18, Version::Draft22] {
			let session = ScriptedSession::new(vec![0x10, 0, 2, 0, 0]);
			let mut stream = Stream::open(&mut crate::driver::Transport::new(session.clone()), version)
				.await
				.unwrap();
			let mut messages = Messages::new(crate::driver::Transport::new(session.clone()), false);
			let mut finished = false;
			assert!(
				futures::poll!(kio::wait(|waiter| poll_cancel(
					&mut stream,
					&mut finished,
					&mut messages,
					version,
					waiter,
				)))
				.is_pending(),
				"{version}"
			);
			assert!(session.log.closes().is_empty());
			// A second GOAWAY is still session-fatal while the response is pending.
			session.push(&[0x10, 0, 2, 0, 0]);
			assert!(matches!(
				futures::poll!(kio::wait(|waiter| poll_cancel(
					&mut stream,
					&mut finished,
					&mut messages,
					version,
					waiter,
				))),
				Poll::Ready(Err(Error::ProtocolViolation))
			));
			assert_eq!(session.log.closes()[0].0, 0x03);
		}
	}

	#[test]
	fn delivery_timeouts_on_subscription_updates_are_accepted() {
		for version in [Version::Draft18, Version::Draft22] {
			// Request ID 2; OBJECT_DELIVERY_TIMEOUT=7, SUBGROUP_DELIVERY_TIMEOUT=9.
			let mut buf = bytes::Bytes::from_static(&[2, 2, 0x02, 7, 0x04, 9]);
			let update = crate::coding::decode_buf(&mut buf, version, Update::decode_body).unwrap();
			assert!(update.refused.is_none(), "{version}: {:?}", update.refused);
		}
	}

	#[test]
	fn track_property_filter_on_subscription_update_is_a_protocol_violation() {
		let version = Version::Draft22;
		let mut buf = bytes::Bytes::from_static(&[2, 1, 0x29, 2, 0, 0]);
		let error = crate::coding::decode_buf(&mut buf, version, Update::decode_body).unwrap_err();
		assert_eq!(
			super::super::error::to_session_code(&crate::SessionError::from(&Error::from(error))),
			0x03
		);
	}

	/// From draft-22 a SUBSCRIBE_TRACKS update may carry TRACK_PROPERTY_FILTER (0x29), so it
	/// decodes there, to be refused per request, while a SUBSCRIBE_NAMESPACE's still closes.
	#[test]
	fn track_property_filter_on_listing_update_decodes() {
		// REQUEST_UPDATE, 6 bytes: Request ID 2, one parameter: 0x29 with a two-byte value.
		const FRAME: &[u8] = &[0x02, 0x00, 0x06, 2, 1, 0x29, 2, 0, 0];
		let version = Version::Draft22;
		let mut buf = bytes::Bytes::from_static(FRAME);
		let update =
			crate::coding::decode_buf(&mut buf, version, |r, v| NamespaceUpdate::<true>::decode(r, v)).unwrap();
		assert!(update.refused.is_none(), "{version}: {:?}", update.refused);
		let mut buf = bytes::Bytes::from_static(FRAME);
		assert!(crate::coding::decode_buf(&mut buf, version, |r, v| NamespaceUpdate::<false>::decode(r, v)).is_err());
		let mut buf = bytes::Bytes::from_static(FRAME);
		assert!(
			crate::coding::decode_buf(&mut buf, Version::Draft18, |r, v| NamespaceUpdate::<true>::decode(r, v))
				.is_err()
		);
	}

	/// TRACK_NAMESPACE_PREFIX (0x34) updates a SUBSCRIBE_NAMESPACE or SUBSCRIBE_TRACKS only,
	/// so on a subscription's REQUEST_UPDATE it closes the session instead of being acked.
	#[test]
	fn namespace_prefix_on_subscription_update_closes() {
		// Request ID 2, one parameter: TRACK_NAMESPACE_PREFIX, a one-field namespace "a".
		let mut buf = bytes::Bytes::from_static(&[0x02, 0x01, 0x34, 0x01, 0x01, b'a']);
		assert!(crate::coding::decode_buf(&mut buf, Version::Draft18, Update::decode_body).is_err());

		// The same update with only a priority is accepted.
		let mut buf = bytes::Bytes::from_static(&[0x02, 0x01, 0x20, 0x05]);
		let update = crate::coding::decode_buf(&mut buf, Version::Draft18, Update::decode_body).unwrap();
		assert_eq!(update.priority, Some(5));
		assert!(update.refused.is_none());
	}
}
