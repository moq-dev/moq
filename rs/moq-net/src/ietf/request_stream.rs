use std::task::{Context, Poll, ready};

use super::{Message, Version};
use crate::{
	Error,
	coding::{Decode, DecodeError, Decoder, Stream},
};

pub(super) fn fin_cancels(version: Version) -> bool {
	matches!(
		version,
		Version::Draft14 | Version::Draft15 | Version::Draft16 | Version::Draft17 | Version::Draft18
	)
}

/// Watch abrupt cancellation after the requester has finished sending messages.
pub(super) fn poll_cancel<S: crate::transport::poll::Session>(
	stream: &mut Stream<S, Version>,
	finished: &mut bool,
	version: Version,
	cx: &mut Context<'_>,
) -> Poll<Result<(), Error>> {
	if !*finished {
		let closed = match version {
			Version::Draft14 | Version::Draft15 | Version::Draft16 => poll_legacy_end(stream, cx),
			_ => stream.reader.poll_closed(cx),
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
pub(super) fn poll_withdrawn<S: crate::transport::poll::Session>(
	stream: &mut Stream<S, Version>,
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
/// An update modifies the request rather than ending it, so it is skipped. Any other
/// message is `Err(Cancel)`, and the stream finishing is `Ok(())`.
pub(super) fn poll_legacy_end<S: crate::transport::poll::Session>(
	stream: &mut Stream<S, Version>,
	cx: &mut Context<'_>,
) -> Poll<Result<(), Error>> {
	loop {
		match ready!(stream.reader.poll_decode_maybe::<FollowUp>(cx))? {
			Some(FollowUp::Update(_)) => {}
			Some(FollowUp::End) => return Poll::Ready(Err(Error::Cancel)),
			None => return Poll::Ready(Ok(())),
		}
	}
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
		let update = r.varint()? == super::SubscribeUpdate::ID;
		let body = super::Body::decode(r, version)?;
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
		Ok(Self(id, super::Body::decode(r, version)?))
	}
}

/// A message following the request on a request stream: `T`, or from draft 18 a GOAWAY
/// migrating this one request (draft-21 section 9.2).
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

/// The GOAWAYs the peer sent on one request stream (draft-21 section 9.2).
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
}

/// Refuse a modern request with its complete REQUEST_ERROR, then finish the response.
pub(super) async fn reject<S: crate::transport::poll::Session>(
	mut stream: Stream<S, Version>,
	error: super::RequestError<'_>,
) -> Result<(), Error> {
	stream.writer.varint(super::RequestError::ID).await?;
	stream.writer.encode(&error).await?;
	// A peer dropping the response after reading the refusal is a normal end.
	let _ = stream.writer.close().await;
	Ok(())
}

/// A framed subscription update, preserving omitted preferences.
#[derive(Debug)]
pub(super) struct Update {
	/// The first Request ID: the update's own on drafts 14-16, which their answer names.
	pub(super) request_id: super::RequestId,
	pub(super) priority: Option<u8>,
	/// Why the update is refused, which fails the subscription: [`Error::Unsupported`], a
	/// [`DecodeError::MalformedAuthToken`] for tokens that repeat, or a
	/// [`DecodeError::MultipleAuthTokens`] for more than one token.
	pub(super) refused: Option<Error>,
	/// The AUTHORIZATION TOKEN renewing the request's credential.
	pub(super) authorization_token: Option<super::token::RequestToken>,
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
		use super::{Fill, Filter, Location, Opaque, Parameters, RequestId};
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
				let tokens = Parameters::skip_request(data)?;
				if !data.is_empty() {
					return Err(DecodeError::InvalidValue);
				}
				let (refused, authorization_token) = Self::refused(tokens, !forward);
				return Ok(Self {
					request_id,
					priority: Some(priority),
					refused,
					authorization_token,
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
			0x23 => fill: Option<Fill> where Filter::is_draft20(version),
			0x25 => subgroup_filter: Vec<Opaque> where super::subscribe::has_range_filters(version),
			0x26 => object_filter: Vec<Opaque> where super::subscribe::has_range_filters(version),
			0x27 => priority_filter: Vec<Opaque> where super::subscribe::has_range_filters(version),
			0x28 => property_filter: Vec<Opaque> where super::subscribe::has_range_filters(version),
			0x29 => track_filter: Vec<Opaque> where super::subscribe::has_range_filters(version),
			0x32 => new_group: Option<u64> where !matches!(version, Version::Draft14 | Version::Draft15),
		);
		if !data.is_empty() {
			return Err(DecodeError::InvalidValue);
		}
		// TRACK_PROPERTY_FILTER belongs only to SUBSCRIBE_TRACKS and its updates.
		if !track_filter.is_empty() {
			return Err(DecodeError::InvalidValue);
		}
		let unsupported = forward == Some(false)
			|| filter.is_some()
			|| fill.is_some()
			|| !subgroup_filter.is_empty()
			|| !object_filter.is_empty()
			|| !priority_filter.is_empty()
			|| !property_filter.is_empty()
			|| new_group.is_some();
		let (refused, authorization_token) = Self::refused(token, unsupported);
		Ok(Self {
			request_id,
			priority,
			refused,
			authorization_token,
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
		let size = r.u16()? as usize;
		let mut data = r.sub(size)?;
		// The complete frame is present; a short field inside it is malformed.
		Self::decode_body(&mut data, version).map_err(DecodeError::complete)
	}
}

/// The namespace-only parameters of a modern REQUEST_UPDATE.
#[derive(Debug)]
pub(super) struct NamespaceUpdate {
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

impl Decode<Version> for NamespaceUpdate {
	fn decode(r: &mut Decoder<'_>, version: Version) -> Result<Self, DecodeError> {
		if r.varint()? != super::SubscribeUpdate::ID {
			return Err(DecodeError::InvalidValue);
		}
		let size = r.u16()? as usize;
		let mut data = r.sub(size)?;
		let mut decode = || {
			let request_id = super::RequestId::decode(&mut data, version)?;
			decode_params!(&mut data, version,
				0x03 => token: Vec<super::token::RequestToken>,
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
		decode().map_err(DecodeError::complete)
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn delivery_timeouts_on_subscription_updates_are_accepted() {
		for version in [Version::Draft18, Version::Draft21, Version::Draft22] {
			// Request ID 2; OBJECT_DELIVERY_TIMEOUT=7, SUBGROUP_DELIVERY_TIMEOUT=9.
			let mut buf = bytes::Bytes::from_static(&[2, 2, 0x02, 7, 0x04, 9]);
			let update = crate::coding::decode_buf(&mut buf, version, Update::decode_body).unwrap();
			assert!(update.refused.is_none(), "{version}: {:?}", update.refused);
		}
	}

	#[test]
	fn track_property_filter_on_subscription_update_is_a_protocol_violation() {
		for version in [Version::Draft19, Version::Draft21, Version::Draft22] {
			let mut buf = bytes::Bytes::from_static(&[2, 1, 0x29, 2, 0, 0]);
			let error = crate::coding::decode_buf(&mut buf, version, Update::decode_body).unwrap_err();
			assert_eq!(
				super::super::error::to_session_code(&crate::SessionError::from(&Error::from(error))),
				0x03
			);
		}
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
