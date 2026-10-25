use crate::driver::Transport;
use crate::transport::poll::Session as _;
use std::task::{Context, Poll, ready};

use super::{Message, Version};
use crate::{
	Error,
	coding::{Decode, DecodeError, Decoder, Stream},
};

/// Legacy receives are framed once by the physical multiplexer, not again by
/// the virtual request decoder. Encodes still count at their typed codec.
pub(super) fn with_stats(
	stream: Stream<Transport, Version>,
	stats: &crate::stats::Session,
	version: Version,
) -> Stream<Transport, Version> {
	let mut stream = stream.with_stats(stats);
	if matches!(version, Version::Draft14 | Version::Draft15 | Version::Draft16) {
		stream.reader = stream.reader.with_stats(&Default::default());
	}
	stream
}

pub(super) fn fin_cancels(version: Version) -> bool {
	matches!(
		version,
		Version::Draft14 | Version::Draft15 | Version::Draft16 | Version::Draft17 | Version::Draft18
	)
}

/// Watch abrupt cancellation after the requester has finished sending messages.
pub(super) fn poll_cancel(
	stream: &mut Stream<Transport, Version>,
	finished: &mut bool,
	messages: &mut Messages,
	version: Version,
	cx: &mut Context<'_>,
) -> Poll<Result<(), Error>> {
	if !*finished {
		let closed = match version {
			Version::Draft14 | Version::Draft15 | Version::Draft16 => poll_legacy_end(stream, version, cx),
			_ => messages.poll_end(&mut stream.reader, cx),
		};
		match closed {
			Poll::Ready(Ok(())) if !fin_cancels(version) => *finished = true,
			Poll::Ready(result) => return Poll::Ready(result),
			Poll::Pending => {}
		}
	}
	// Virtual writers on drafts 14-16 are already closed while their control request lives.
	if !matches!(version, Version::Draft14 | Version::Draft15 | Version::Draft16) {
		return stream.writer.poll_closed(cx);
	}
	Poll::Pending
}

/// Watch for the requester ending a request it has not been answered on yet, without consuming
/// what it sent meanwhile: a waiting message (an update, say) ends the watch on the reader and
/// is read once the request is answered. A withdrawal queued behind an update is therefore seen
/// only then; until the verifier answers, the requester holds nothing but its own slot.
pub(super) fn poll_withdrawn(
	stream: &mut Stream<Transport, Version>,
	version: Version,
	cx: &mut Context<'_>,
) -> Poll<()> {
	// Drafts 14-16 deliver the terminal message (UNSUBSCRIBE, PUBLISH_NAMESPACE_DONE) ahead of
	// the FIN, so a waiting message that is not an update already withdraws the request. Their
	// virtual writers are closed while the control request lives, so only the reader counts.
	if matches!(version, Version::Draft14 | Version::Draft15 | Version::Draft16) {
		return match stream.reader.poll_varint_peek(cx) {
			Poll::Ready(Ok(id)) if id == super::SubscribeUpdate::ID => Poll::Pending,
			Poll::Ready(_) => Poll::Ready(()),
			Poll::Pending => Poll::Pending,
		};
	}
	if stream.writer.poll_closed(cx).is_ready() {
		return Poll::Ready(());
	}
	match stream.reader.poll_has_more(cx) {
		Poll::Ready(Ok(false)) if fin_cancels(version) => Poll::Ready(()),
		Poll::Ready(Err(_)) => Poll::Ready(()),
		_ => Poll::Pending,
	}
}

/// Poll a draft 14-16 virtual request stream until the requester ends the request.
///
/// An update modifies the request rather than ending it, and the requests watched here
/// apply none, so each is refused with [`refuse_update`] and the request stays open. Any
/// other message is `Err(Cancel)`, and the stream finishing is `Ok(())`.
pub(super) fn poll_legacy_end(
	stream: &mut Stream<Transport, Version>,
	version: Version,
	cx: &mut Context<'_>,
) -> Poll<Result<(), Error>> {
	loop {
		// A refusal goes out before the next message is read.
		ready!(stream.writer.poll_flush(cx))?;
		match ready!(stream.reader.poll_decode_maybe::<FollowUp>(cx))? {
			Some(FollowUp::Update(body)) => refuse_update(&mut stream.writer, &body, version)?,
			Some(FollowUp::End) => return Poll::Ready(Err(Error::Cancel)),
			None => return Poll::Ready(Ok(())),
		}
	}
}

/// Buffer the answer to a draft 14-16 update aimed at a request that takes none.
///
/// Draft 14 answers no update. Drafts 15 and 16 answer each with exactly one REQUEST_OK or
/// REQUEST_ERROR naming the update's own Request ID, and a refusal leaves any request but a
/// subscription standing, so NOT_SUPPORTED is the whole answer.
pub(super) fn refuse_update<W: crate::transport::poll::SendStream>(
	writer: &mut crate::coding::Writer<W, Version>,
	body: &super::Body,
	version: Version,
) -> Result<(), Error> {
	if version == Version::Draft14 {
		return Ok(());
	}
	let request_id = super::RequestId::decode(&mut body.decoder(version), version).map_err(DecodeError::complete)?;
	writer.buffer_message(
		super::RequestError::ID,
		&super::RequestError {
			request_id: Some(request_id),
			error_code: super::error::request::to_code(
				&Error::Unsupported,
				super::error::request::Kind::Subscribe,
				version,
			),
			reason_phrase: "update not supported".into(),
			retry_interval: 0,
		},
	)
}

/// A message the adapter routed onto a draft 14-16 request after the request itself.
///
/// Those drafts carry every request on the control stream, and the adapter delivers
/// each message to the request it names.
#[derive(Debug)]
pub(super) enum FollowUp {
	/// SUBSCRIBE_UPDATE, or REQUEST_UPDATE on draft 16, still undecoded.
	Update(super::Body),
	/// UNSUBSCRIBE, FETCH_CANCEL, or any other message, which ends the request.
	End,
}

impl Decode<Version> for FollowUp {
	fn decode(r: &mut Decoder<'_>, version: Version) -> Result<Self, DecodeError> {
		let id = r.varint()?;
		let update = id == super::SubscribeUpdate::ID;
		let start = r.position();
		let body = super::Body::decode(r, version)?;
		r.control = Some(super::message::control(id, version));
		r.uncounted = start;
		Ok(match update {
			true => Self::Update(body),
			false => Self::End,
		})
	}
}

/// One whole `[type][length][body]` message off a request stream, decoded at once so a read
/// dropped part-way consumes nothing.
#[derive(Debug)]
pub(super) struct Framed(pub(super) u64, pub(super) super::Body);

impl Decode<Version> for Framed {
	fn decode(r: &mut Decoder<'_>, version: Version) -> Result<Self, DecodeError> {
		let id = r.varint()?;
		let start = r.position();
		let body = super::Body::decode(r, version)?;
		r.control = Some(super::message::control(id, version));
		r.uncounted = start;
		Ok(Self(id, body))
	}
}

/// A message following the request on a request stream: `T`, or from draft 18 a GOAWAY
/// migrating this one request (draft-22 section 9.2).
#[derive(Debug)]
pub(super) enum Next<T> {
	Message(T),
	/// `redirect` is whether it names a new session, which only a server may do.
	GoAway {
		redirect: bool,
	},
}

impl<T: Decode<Version>> Decode<Version> for Next<T> {
	fn decode(r: &mut Decoder<'_>, version: Version) -> Result<Self, DecodeError> {
		if !matches!(
			version,
			Version::Draft14 | Version::Draft15 | Version::Draft16 | Version::Draft17
		) {
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
	pub fn poll_next<T: Decode<Version> + std::fmt::Debug, R: crate::transport::poll::RecvStream>(
		&mut self,
		reader: &mut crate::coding::Reader<R, Version>,
		cx: &mut Context<'_>,
	) -> Poll<Result<Option<T>, Error>> {
		loop {
			match ready!(reader.poll_decode_maybe::<Next<T>>(cx))? {
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
		cx: &mut Context<'_>,
	) -> Poll<Result<Option<Framed>, Error>> {
		self.goaways.poll_next(reader, cx).map_err(|err| self.failed(err))
	}

	pub async fn next<R: crate::transport::poll::RecvStream>(
		&mut self,
		reader: &mut crate::coding::Reader<R, Version>,
	) -> Result<Option<Framed>, Error> {
		std::future::poll_fn(|cx| self.poll_next(reader, cx)).await
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
		cx: &mut Context<'_>,
	) -> Poll<Result<(), Error>> {
		match ready!(self.poll_next(reader, cx))? {
			None => Poll::Ready(Ok(())),
			Some(_) => Poll::Ready(Err(self.failed(Error::UnexpectedMessage))),
		}
	}
}

/// Refuse a modern request with its complete REQUEST_ERROR, then finish the response.
pub(super) async fn reject(
	mut stream: Stream<Transport, Version>,
	error: super::RequestError<'_>,
) -> Result<(), Error> {
	stream.writer.encode_message(&error).await?;
	// A peer dropping the response after reading the refusal is a normal end.
	let _ = stream.writer.close().await;
	Ok(())
}

/// Write the error refusing a `kind` request with `err`, as the negotiated draft spells it.
///
/// Each draft-14 error message has the REQUEST_ERROR body that draft-15 folded them into, so
/// only the message type changes; draft-17 then drops the Request ID.
pub(super) async fn write_error<W: crate::transport::poll::SendStream>(
	writer: &mut crate::coding::Writer<W, Version>,
	kind: super::error::request::Kind,
	request_id: super::RequestId,
	err: &Error,
	reason: &str,
	version: Version,
) -> Result<(), Error> {
	use super::error::request::Kind;

	let id = match (version, kind) {
		(Version::Draft14, Kind::Subscribe) => super::SubscribeError::ID,
		(Version::Draft14, Kind::Fetch) => super::FetchError::ID,
		(Version::Draft14, Kind::Publish) => super::PublishError::ID,
		(Version::Draft14, Kind::PublishNamespace) => super::PublishNamespaceError::ID,
		(Version::Draft14, Kind::SubscribeNamespace) => super::SubscribeNamespaceError::ID,
		(Version::Draft14, Kind::TrackStatus) => super::TRACK_STATUS_ERROR_14,
		_ => super::RequestError::ID,
	};
	writer.buffer_message(
		id,
		&super::RequestError {
			request_id: matches!(version, Version::Draft14 | Version::Draft15 | Version::Draft16).then_some(request_id),
			error_code: super::error::request::to_code(err, kind, version),
			reason_phrase: reason.into(),
			retry_interval: 0,
		},
	)?;
	std::future::poll_fn(|cx| writer.poll_flush(cx)).await
}

/// A framed subscription update, preserving omitted preferences.
#[derive(Debug)]
pub(super) struct Update {
	/// The first Request ID: the update's own on drafts 14-16, which their answer names.
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
	/// NEW_GROUP_REQUEST (draft-15+), which only a track advertising DYNAMIC_GROUPS takes.
	pub(super) new_group: Option<u64>,
	/// FORWARD, `None` when unchanged.
	pub(super) forward: Option<bool>,
	/// The new Location Filter, which only a paused PUBLISH subscription resuming takes.
	pub(super) filter: Option<super::Filter>,
	/// FILL_PARAMETERS (draft-22), which only a paused PUBLISH subscription resuming takes.
	pub(super) fill: Option<super::Fill>,
}

impl Update {
	/// Decode a draft 14-16 update the adapter routed onto its subscription.
	pub(super) fn decode_legacy(body: &super::Body, version: Version) -> Result<Self, Error> {
		let mut data = body.decoder(version);
		// The body is complete, so running short inside it is malformed.
		Self::decode_body(&mut data, version)
			.map_err(DecodeError::complete)
			.map_err(Into::into)
	}

	/// Decode the message body, after its type and length.
	fn decode_body(data: &mut Decoder<'_>, version: Version) -> Result<Self, DecodeError> {
		use super::{Fill, Filter, Location, Opaque, RequestId};
		let request_id = RequestId::decode(data, version)?;
		match version {
			Version::Draft14 => {
				// The subscription it updates, which the adapter already routed by.
				RequestId::decode(data, version)?;
				// The range is left as it is: every field is mandatory, so a narrowed range
				// cannot be told from a repeated one, and draft 14 accepts extra objects as
				// the worst outcome of an update.
				Location::decode(data, version)?;
				let _end_group = data.varint()?;
				let priority = data.u8()?;
				let forward = data.bool()?;
				decode_params!(data, version,
					0x03 => tokens: Vec<super::token::RequestToken>,
					super::max_age::PARAMETER => max_age: Option<u64>,
				);
				if !data.is_empty() {
					return Err(DecodeError::InvalidValue);
				}
				let (refused, authorization_token) = Self::refused(tokens, false);
				return Ok(Self {
					request_id,
					priority: Some(priority),
					max_age: super::max_age::validate(max_age)?,
					refused,
					authorization_token,
					new_group: None,
					forward: Some(forward),
					filter: None,
					fill: None,
				});
			}
			// The request it updates, which the adapter already routed by.
			Version::Draft15 | Version::Draft16 => {
				RequestId::decode(data, version)?;
			}
			Version::Draft17 => {
				data.varint()?;
			}
			_ => {}
		}
		decode_params!(data, version,
			0x02 => object_timeout: Option<u64>,
			0x03 => token: Vec<super::token::RequestToken>,
			0x06 => subgroup_timeout: Option<u64> where !matches!(version, Version::Draft14 | Version::Draft15 | Version::Draft16 | Version::Draft17),
			0x10 => forward: Option<bool>,
			0x20 => priority: Option<u8>,
			0x21 => filter: Option<Filter>,
			0x23 => fill: Option<Fill> where Filter::is_draft22(version),
			0x25 => subgroup_filter: Vec<Opaque> where super::subscribe::has_range_filters(version),
			0x26 => object_filter: Vec<Opaque> where super::subscribe::has_range_filters(version),
			0x27 => priority_filter: Vec<Opaque> where super::subscribe::has_range_filters(version),
			0x28 => property_filter: Vec<Opaque> where super::subscribe::has_range_filters(version),
			0x29 => track_filter: Vec<Opaque> where super::subscribe::has_range_filters(version),
			0x32 => new_group: Option<u64> where super::subscribe::has_new_group_request(version),
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

impl Decode<Version> for Update {
	fn decode(r: &mut Decoder<'_>, version: Version) -> Result<Self, DecodeError> {
		if r.varint()? != super::SubscribeUpdate::ID {
			return Err(DecodeError::InvalidValue);
		}
		let start = r.position();
		let size = r.u16()? as usize;
		let mut data = r.sub(size)?;
		// The complete frame is present; a short field inside it is malformed.
		let update = Self::decode_body(&mut data, version).map_err(DecodeError::complete)?;
		r.control = Some(super::message::control(super::SubscribeUpdate::ID, version));
		r.uncounted = start;
		Ok(update)
	}
}

/// The namespace-only parameters of a modern REQUEST_UPDATE. `TRACKS` marks one on a
/// SUBSCRIBE_TRACKS, which from draft-22 may also carry TRACK_PROPERTY_FILTER and from
/// draft-22 FORWARD.
#[derive(Debug)]
pub(super) struct NamespaceUpdate<const TRACKS: bool = false> {
	pub(super) request_id: super::RequestId,
	pub(super) prefix: Option<crate::PathOwned>,
	pub(super) refused: Option<Error>,
}

#[derive(Debug)]
struct NamespacePrefix(crate::PathOwned);

impl super::parameters::Param for NamespacePrefix {
	fn param_encode(&self, w: &mut crate::coding::Encoder<'_>, _: Version) -> Result<(), crate::coding::EncodeError> {
		super::namespace::encode_namespace(w, &self.0)
	}
	fn param_decode(r: &mut Decoder<'_>, _: Version) -> Result<Self, DecodeError> {
		Ok(Self(super::namespace::decode_namespace(r)?))
	}
}

impl<const TRACKS: bool> Decode<Version> for NamespaceUpdate<TRACKS> {
	fn decode(r: &mut Decoder<'_>, version: Version) -> Result<Self, DecodeError> {
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
		r.control = Some(super::message::control(super::SubscribeUpdate::ID, version));
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
		use crate::stats::{Control, Registry, Tier};
		let registry = Registry::new(Default::default());
		let stats = registry.tier(Tier::default()).session("");
		let version = Version::Draft22;
		let session = ScriptedSession::new(vec![0x10, 0, 2, 0, 0, 0x07, 0, 0]);
		let mut stream = with_stats(
			Stream::open(&mut crate::driver::Transport::new(session.clone()), version)
				.await
				.unwrap(),
			&stats,
			version,
		);
		let mut messages = Messages::new(crate::driver::Transport::new(session.clone()), true);
		assert_eq!(messages.message(&mut stream.reader).await.unwrap().0, 0x07);
		let rx = |class| {
			let rows = registry.snapshot().control();
			rows.into_iter().find(|row| row.1 == class).map(|row| row.2.rx)
		};
		assert_eq!(rx(Control::Goaway), Some(4));
		assert_eq!(rx(Control::Other), Some(2));
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
				futures::poll!(std::future::poll_fn(|cx| poll_cancel(
					&mut stream,
					&mut finished,
					&mut messages,
					version,
					cx,
				)))
				.is_pending(),
				"{version}"
			);
			assert!(session.log.closes().is_empty());
			// A second GOAWAY is still session-fatal while the response is pending.
			session.push(&[0x10, 0, 2, 0, 0]);
			assert!(matches!(
				futures::poll!(std::future::poll_fn(|cx| poll_cancel(
					&mut stream,
					&mut finished,
					&mut messages,
					version,
					cx,
				))),
				Poll::Ready(Err(Error::ProtocolViolation))
			));
			assert_eq!(session.log.closes()[0].0, 0x03);
		}
	}

	/// On drafts 14-16 an update aimed at a request that applies none, such as a FETCH or
	/// SUBSCRIBE_NAMESPACE, keeps the request open. Drafts 15 and 16 refuse it with a
	/// REQUEST_ERROR naming the update, and a later cancellation still ends the request.
	#[moq_clock::test]
	async fn legacy_update_is_answered_without_ending_the_request() {
		use crate::lite::test_transport::ScriptedSession;

		for version in [Version::Draft14, Version::Draft15, Version::Draft16] {
			// Own Request ID 10, target 0, no parameters. Draft 14 carries its fixed start,
			// end group, priority and forward first.
			let body: &[u8] = match version {
				Version::Draft14 => &[10, 0, 0, 0, 0, 128, 1, 0],
				_ => &[10, 0, 0],
			};
			let mut update = vec![super::super::SubscribeUpdate::ID as u8, 0, body.len() as u8];
			update.extend_from_slice(body);
			let session = ScriptedSession::new(update);
			let mut stream = Stream::open(&mut crate::driver::Transport::new(session.clone()), version)
				.await
				.unwrap();
			let mut finished = false;
			let mut messages = Messages::new(crate::driver::Transport::new(session.clone()), false);

			let polled = futures::poll!(std::future::poll_fn(|cx| poll_cancel(
				&mut stream,
				&mut finished,
				&mut messages,
				version,
				cx
			)));
			assert!(polled.is_pending(), "{version}: the update ended the request");

			let written = session.log.writes.lock().unwrap().clone();
			match version {
				Version::Draft14 => assert!(written.is_empty(), "{version}: {written:?}"),
				// [type, length (2), Request ID, NOT_SUPPORTED, ...]
				_ => assert_eq!(
					(written[0], written[3], written[4]),
					(super::super::RequestError::ID as u8, 10, 0x3),
					"{version}: {written:?}"
				),
			}

			// UNSUBSCRIBE, or any message but an update, ends the request.
			session.push(&[super::super::Unsubscribe::ID as u8, 0, 1, 0]);
			let polled = futures::poll!(std::future::poll_fn(|cx| poll_cancel(
				&mut stream,
				&mut finished,
				&mut messages,
				version,
				cx
			)));
			assert!(
				matches!(polled, Poll::Ready(Err(Error::Cancel))),
				"{version}: {polled:?}"
			);
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
		let update = crate::coding::decode_buf(&mut buf, version, NamespaceUpdate::<true>::decode).unwrap();
		assert!(update.refused.is_none(), "{version}: {:?}", update.refused);
		let mut buf = bytes::Bytes::from_static(FRAME);
		assert!(crate::coding::decode_buf(&mut buf, version, NamespaceUpdate::<false>::decode).is_err());
		let mut buf = bytes::Bytes::from_static(FRAME);
		assert!(crate::coding::decode_buf(&mut buf, Version::Draft18, NamespaceUpdate::<true>::decode).is_err());
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

	/// Draft-14's per-request error messages share REQUEST_ERROR's body, so the shared writer
	/// must put the same bytes on the wire as each legacy message does.
	#[moq_clock::test]
	async fn draft14_errors_match_their_legacy_messages() {
		use super::super::error::request::{Kind, to_code};
		use super::super::{
			FetchError, PublishError, PublishNamespaceError, RequestId, SubscribeError, SubscribeNamespaceError,
		};
		use crate::lite::test_transport::{Log, SinkSend};

		let version = Version::Draft14;
		let (request_id, err, reason) = (RequestId(6), Error::NotFound, "gone");

		for kind in [
			Kind::Subscribe,
			Kind::Fetch,
			Kind::Publish,
			Kind::PublishNamespace,
			Kind::SubscribeNamespace,
			Kind::TrackStatus,
		] {
			let shared = Log::default();
			let mut writer = crate::coding::Writer::new(
				crate::transport::boxed::SendStream::new(SinkSend::new(shared.clone())),
				version,
			);
			write_error(&mut writer, kind, request_id, &err, reason, version)
				.await
				.unwrap();

			let legacy = Log::default();
			let mut writer = crate::coding::Writer::new(
				crate::transport::boxed::SendStream::new(SinkSend::new(legacy.clone())),
				version,
			);
			let error_code = to_code(&err, kind, version);
			let reason_phrase = reason.into();
			match kind {
				Kind::Subscribe => {
					writer
						.encode_message(&SubscribeError {
							request_id,
							error_code,
							reason_phrase,
						})
						.await
				}
				Kind::Fetch => {
					writer
						.encode_message(&FetchError {
							request_id,
							error_code,
							reason_phrase,
						})
						.await
				}
				Kind::Publish => {
					writer
						.encode_message(&PublishError {
							request_id,
							error_code,
							reason_phrase,
						})
						.await
				}
				Kind::PublishNamespace => {
					writer
						.encode_message(&PublishNamespaceError {
							request_id,
							error_code,
							reason_phrase,
						})
						.await
				}
				Kind::SubscribeNamespace => {
					writer
						.encode_message(&SubscribeNamespaceError {
							request_id,
							error_code,
							reason_phrase,
						})
						.await
				}
				// TRACK_STATUS_ERROR has the SUBSCRIBE_ERROR body under its own type.
				Kind::TrackStatus => {
					writer.buffer_varint(super::super::TRACK_STATUS_ERROR_14).unwrap();
					writer
						.encode(&SubscribeError {
							request_id,
							error_code,
							reason_phrase,
						})
						.await
				}
			}
			.unwrap();

			assert_eq!(
				*shared.writes.lock().unwrap(),
				*legacy.writes.lock().unwrap(),
				"{kind:?}"
			);
		}
	}
}
