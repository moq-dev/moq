//! IETF moq-transport TRACK_STATUS and its answer.

use std::borrow::Cow;

use num_enum::{IntoPrimitive, TryFromPrimitive};

use crate::{
	Path,
	coding::*,
	ietf::{Filter, Location, Properties, RequestId, Subscribe},
};

use super::Message;
use super::namespace::encode_track;

use super::Version;

/// TrackStatus message (0x0d), with the same wire format as SUBSCRIBE. The response is
/// REQUEST_OK.
#[derive(Clone, Debug)]
pub struct TrackStatus<'a> {
	pub request_id: RequestId,
	pub track_namespace: Path,
	pub track_name: Cow<'a, str>,
	/// Whether the requester wants Track Properties on the answer (INCLUDE_PROPERTIES,
	/// draft-22).
	pub properties_wanted: bool,
	/// The publisher instance the requester expects (MoQ Epoch).
	pub epoch: Option<crate::Epoch>,
}

impl TrackStatus<'_> {
	/// Copy the borrowed track name, so the request outlives the bytes it came from.
	pub fn into_owned(self) -> TrackStatus<'static> {
		TrackStatus {
			request_id: self.request_id,
			track_namespace: self.track_namespace,
			track_name: Cow::Owned(self.track_name.into_owned()),
			properties_wanted: self.properties_wanted,
			epoch: self.epoch,
		}
	}
}

impl<'a> Message<'a> for TrackStatus<'a> {
	const ID: u64 = 0x0d;

	fn encode_msg(&self, w: &mut Encoder<'_>, version: Version) -> Result<(), EncodeError> {
		self.request_id.encode(w, version)?;
		encode_track(w, &self.track_namespace, &self.track_name)?;

		// Only the opt-out is worth bytes, and an older peer would read the
		// parameter as unknown, which is a protocol violation.
		let include_properties = (!self.properties_wanted && Filter::is_draft20(version)).then_some(false);
		let epoch = self.epoch.clone();
		encode_params!(w, version,
			0x35 => include_properties,
			super::epoch::EPOCH_PARAM => epoch,
		);
		Ok(())
	}

	/// Every draft defines TRACK_STATUS as identical to SUBSCRIBE, so it decodes as one and
	/// keeps what names the track and shapes the answer. The rest goes unread: there is no
	/// subscription for it to configure.
	fn decode_msg(r: &mut Decoder<'a>, version: Version) -> Result<Self, DecodeError> {
		let subscribe = Subscribe::decode_msg(r, version)?;
		Ok(Self {
			request_id: subscribe.request_id,
			track_namespace: subscribe.track_namespace,
			track_name: subscribe.track_name,
			properties_wanted: subscribe.properties_wanted,
			epoch: subscribe.epoch,
		})
	}
}

/// TRACK_STATUS_OK: what a SUBSCRIBE_OK would say about the track, with no subscription,
/// sent as a REQUEST_OK with Track Properties.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TrackStatusOk {
	/// The largest Location in the track (LARGEST_OBJECT), absent while it has no content.
	pub largest: Option<Location>,

	/// The Track Properties.
	pub properties: Properties,
}

impl<'a> Message<'a> for TrackStatusOk {
	/// REQUEST_OK.
	const ID: u64 = 0x07;

	fn encode_msg(&self, w: &mut Encoder<'_>, version: Version) -> Result<(), EncodeError> {
		// The parameters SUBSCRIBE_OK carries.
		encode_params!(w, version,
			0x09 => self.largest,
		);

		// Track Properties are the final field, so nothing may follow.
		self.properties.encode(w, version)?;
		Ok(())
	}

	fn decode_msg(r: &mut Decoder<'a>, version: Version) -> Result<Self, DecodeError> {
		// Read as SUBSCRIBE_OK's are: EXPIRES is ignored.
		decode_params!(r, version,
			0x08 => _expires: Option<u64>,
			0x09 => largest: Option<Location>,
		);
		let properties = Properties::decode(r, version)?;

		Ok(Self { largest, properties })
	}
}

#[derive(Clone, Copy, Debug, TryFromPrimitive, IntoPrimitive)]
#[repr(u64)]
pub enum TrackStatusCode {
	InProgress = 0x00,
	NotFound = 0x01,
	NotAuthorized = 0x02,
	Ended = 0x03,
}

impl Encode<Version> for TrackStatusCode {
	fn encode(&self, w: &mut Encoder<'_>, _: Version) -> Result<(), EncodeError> {
		w.varint(u64::from(*self))?;
		Ok(())
	}
}

impl<'a> Decode<'a, Version> for TrackStatusCode {
	fn decode(r: &mut Decoder<'a>, _: Version) -> Result<Self, DecodeError> {
		Self::try_from(r.varint()?).map_err(|_| DecodeError::InvalidValue)
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::ietf::GroupOrder;

	fn encode_message<'m, M: Message<'m>>(msg: &M, version: Version) -> Vec<u8> {
		let mut buf = Vec::new();
		msg.encode_msg(&mut Encoder::new(&mut buf, version.into()), version)
			.unwrap();
		buf.to_vec()
	}

	/// TRACK_STATUS_OK rejects a known parameter on a message that does not define it.
	#[test]
	fn track_status_ok_parameters_follow_the_draft() {
		// GROUP_ORDER = Ascending, then an empty properties block.
		assert!(decode_message::<TrackStatusOk>(&[0x01, 0x22, 0x01], Version::Draft18).is_err());
	}

	fn decode_message<'m, M: Message<'m>>(bytes: &'m [u8], version: Version) -> Result<M, DecodeError> {
		let mut buf = bytes;
		crate::coding::decode_from(&mut buf, version, |r, v| M::decode_msg(r, v))
	}

	#[test]
	fn test_track_status_v18_round_trip() {
		let msg = TrackStatus {
			request_id: RequestId(1),
			track_namespace: Path::from_static("test/ns"),
			track_name: "video".into(),
			properties_wanted: true,
			epoch: None,
		};

		let encoded = encode_message(&msg, Version::Draft18);
		let decoded: TrackStatus = decode_message(&encoded, Version::Draft18).unwrap();

		assert_eq!(decoded.request_id, RequestId(1));
		assert_eq!(decoded.track_namespace.as_str(), "test/ns");
		assert_eq!(decoded.track_name, "video");
	}

	const ALL: [Version; 3] = [Version::Draft18, Version::Draft20, Version::Draft22];

	/// INCLUDE_PROPERTIES only exists from draft-22, so the opt-out is dropped before it,
	/// and the default (1) is never written.
	#[test]
	fn test_track_status_opts_out_of_properties_from_draft_22() {
		for version in ALL {
			for wanted in [true, false] {
				let msg = TrackStatus {
					request_id: RequestId(1),
					track_namespace: Path::from_static("ns"),
					track_name: "video".into(),
					properties_wanted: wanted,
					epoch: None,
				};
				let wire = encode_message(&msg, version);

				let decoded: TrackStatus = decode_message(&wire, version).unwrap_or_else(|e| panic!("{version}: {e}"));
				let expected = wanted || !Filter::is_draft20(version);
				assert_eq!(decoded.properties_wanted, expected, "{version} wanted={wanted}");
			}
		}
	}

	/// TRACK_STATUS_OK carries what SUBSCRIBE_OK would: a REQUEST_OK ending with the Track
	/// Properties.
	#[test]
	fn test_track_status_ok_round_trip() {
		let properties = Properties {
			unknown: Default::default(),
			dynamic_groups: false,
			compress: false,
			max_cache_duration: Some(std::time::Duration::from_secs(30)),
			timescale: Some(crate::Timescale::MILLI),
			priority: Some(7),
			group_order: Some(GroupOrder::Descending),
		};
		let largest = Some(Location { group: 5, object: 2 });

		for version in ALL {
			let msg = TrackStatusOk {
				largest,
				properties: properties.clone(),
			};
			let decoded: TrackStatusOk =
				decode_message(&encode_message(&msg, version), version).unwrap_or_else(|e| panic!("{version}: {e}"));

			let expected = properties.clone();
			assert_eq!(decoded.largest, largest, "{version}");
			assert_eq!(decoded.properties, expected, "{version}");
		}
	}

	/// TRACK_STATUS is identical to SUBSCRIBE on every draft, so it carries whatever
	/// fields and parameters a SUBSCRIBE can. A peer that sends them must still be
	/// answered rather than having its session closed.
	#[test]
	fn test_track_status_carries_subscribe_fields() {
		// Request ID 1, Track Namespace ("live"), Track Name ("video").
		let head: &[u8] = &[
			0x01, 0x01, 0x04, b'l', b'i', b'v', b'e', 0x05, b'v', b'i', b'd', b'e', b'o',
		];

		#[rustfmt::skip]
		let cases: [(Version, &[u8]); 1] = [
			(Version::Draft22, &[
				0x02, // Number of Parameters
				0x03, 0x03, 0x03, 0x00, 0xAA, // AUTHORIZATION TOKEN
				0x32, 0x00, // INCLUDE_PROPERTIES (0x35) = 0
			]),
		];

		for (version, rest) in cases {
			let body = [head, rest].concat();
			let wire = body;
			let mut buf = Decoder::new(&wire, version.into());
			let msg = TrackStatus::decode_msg(&mut buf, version).unwrap_or_else(|e| panic!("{version}: {e}"));
			assert!(buf.is_empty(), "{version}: trailing bytes");
			assert_eq!(msg.track_name, "video", "{version}");
			assert_eq!(msg.properties_wanted, version != Version::Draft22, "{version}");
		}
	}
}
