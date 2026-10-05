use std::task::{Context, Poll};

use super::Version;
use crate::{Error, coding::Stream};

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
		match stream.reader.poll_closed(cx) {
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

/// A framed subscription update, preserving omitted preferences.
#[derive(Debug)]
pub(super) struct Update {
	pub(super) priority: Option<u8>,
	pub(super) unsupported: bool,
}

impl crate::coding::Decode<Version> for Update {
	fn decode(r: &mut crate::coding::Decoder<'_>, version: Version) -> Result<Self, crate::coding::DecodeError> {
		use super::{Fill, Filter, Opaque, RequestId};
		use crate::coding::DecodeError;
		if r.varint()? != 0x02 {
			return Err(DecodeError::InvalidValue);
		}
		let size = r.u16()? as usize;
		let mut data = r.sub(size)?;
		let result = (|| {
			let _id = RequestId::decode(&mut data, version)?;
			if version == Version::Draft17 {
				data.varint()?;
			}
			decode_params!(&mut data, version,
				0x02 => object_timeout: Option<u64>,
				0x03 => token: Vec<Opaque>,
				0x06 => subgroup_timeout: Option<u64>,
				0x10 => forward: Option<bool>,
				0x20 => priority: Option<u8>,
				0x21 => filter: Option<Filter>,
				0x23 => fill: Option<Fill>,
				0x25 => subgroup_filter: Vec<Opaque>,
				0x26 => object_filter: Vec<Opaque>,
				0x27 => priority_filter: Vec<Opaque>,
				0x28 => property_filter: Vec<Opaque>,
				0x29 => track_filter: Vec<Opaque>,
				0x32 => new_group: Option<u64>,
			);
			if !data.is_empty() {
				return Err(DecodeError::InvalidValue);
			}
			Ok(Self {
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
		})();
		// The complete frame is present; a short field inside it is malformed.
		result.map_err(DecodeError::complete)
	}
}
