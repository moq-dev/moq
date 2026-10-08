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

/// A framed subscription update, preserving omitted preferences.
#[derive(Debug)]
pub(super) struct Update {
	/// The first Request ID: the update's own on drafts 14-16, which their answer names.
	pub(super) request_id: super::RequestId,
	pub(super) priority: Option<u8>,
	pub(super) unsupported: bool,
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
				Parameters::skip(data)?;
				if !data.is_empty() {
					return Err(DecodeError::InvalidValue);
				}
				return Ok(Self {
					request_id,
					priority: Some(priority),
					unsupported: !forward,
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
			0x03 => token: Vec<Opaque>,
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
			// Legal on a SUBSCRIBE_NAMESPACE update from draft-18. The message cannot
			// say which request it updates, so the prefix is consumed and not applied.
			0x34 => _prefix: Option<super::parameters::TrackNamespace> where !matches!(version, Version::Draft14 | Version::Draft15 | Version::Draft16 | Version::Draft17),
		);
		if !data.is_empty() {
			return Err(DecodeError::InvalidValue);
		}
		Ok(Self {
			request_id,
			priority,
			unsupported: forward == Some(false)
				|| filter.is_some()
				|| fill.is_some()
				|| object_timeout.is_some()
				|| subgroup_timeout.is_some()
				|| !token.is_empty()
				|| !subgroup_filter.is_empty()
				|| !object_filter.is_empty()
				|| !priority_filter.is_empty()
				|| !property_filter.is_empty()
				|| !track_filter.is_empty()
				|| new_group.is_some(),
		})
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
