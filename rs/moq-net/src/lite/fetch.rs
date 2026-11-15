use std::borrow::Cow;

use crate::{
	Path,
	coding::{Decode, DecodeError, Decoder, Encode, EncodeError, Encoder},
};

use super::{Message, Version};

/// Sent by the subscriber to fetch a specific group from a track.
///
/// Lite03+ only.
#[derive(Clone, Debug)]
pub struct Fetch<'a> {
	pub broadcast: Path,
	/// The instance the subscriber expects; see [`crate::origin::RouteInfo::instance`].
	/// Lite07+ only.
	pub instance: Option<crate::Instance>,
	/// The Node ID of the origin that serves the request, or 0 for whichever the
	/// publisher selects. Lite07+ only.
	pub node: u64,
	/// How many leading segments of the broadcast path make up the prefix of the route
	/// the request follows (lite-07); 0 with no node named.
	pub depth: u64,
	pub track: Cow<'a, str>,
	pub priority: i8,
	/// Reader freshness budget. Lite07+ only; omission uses the publisher's max age alone.
	pub max_delay: Option<std::time::Duration>,
	pub group: u64,
	/// Index of the first frame to return; 0 is the start of the group. Lite06+ only.
	pub start_frame: u64,
	/// Offset of the first byte to return within frame `start_frame`; 0 is the whole
	/// frame. The response still states that frame's full size. Lite07+ only.
	pub start_byte: u64,
	/// Index of the last frame to return (inclusive), or `None` for through the end of
	/// the group. Lite06+ only.
	pub end_frame: Option<u64>,
}

impl Fetch<'_> {
	/// Copy the borrowed track name, so the request outlives the bytes it came from.
	pub fn into_owned(self) -> Fetch<'static> {
		Fetch {
			broadcast: self.broadcast,
			instance: self.instance,
			node: self.node,
			depth: self.depth,
			track: Cow::Owned(self.track.into_owned()),
			priority: self.priority,
			max_delay: self.max_delay,
			group: self.group,
			start_frame: self.start_frame,
			start_byte: self.start_byte,
			end_frame: self.end_frame,
		}
	}
}

impl<'a> Message<'a> for Fetch<'a> {
	const CONTROL: Option<crate::stats::Message> = Some(crate::stats::Message::Fetch);
	fn decode_msg(r: &mut Decoder<'a>, version: Version) -> Result<Self, DecodeError> {
		match version {
			Version::Lite01 | Version::Lite02 => {
				return Err(DecodeError::Version);
			}
			_ => {}
		}

		let broadcast = Path::decode(r, version)?;
		let instance = super::instance::decode_pin(r, version)?;
		let (node, depth) = super::instance::decode_node(r, version, &broadcast)?;
		let track = Cow::Borrowed(r.str()?);
		let priority = super::priority::from_wire(r.u8()?);
		let max_delay = match version.has_fetch_max_delay() {
			true => r.varint_opt()?.map(std::time::Duration::from_millis),
			false => None,
		};
		let group = r.varint()?;

		let start_frame = match version.has_frame_bounds() {
			true => r.varint()?,
			false => 0,
		};
		let start_byte = match version.has_byte_start() {
			true => r.varint()?,
			false => 0,
		};
		let end_frame = match version.has_frame_bounds() {
			true => r.varint_opt()?,
			false => None,
		};
		// A range that ends before it starts can never be served.
		if end_frame.is_some_and(|end| end < start_frame) {
			return Err(DecodeError::InvalidSubscribeLocation);
		}

		Ok(Self {
			broadcast,
			instance,
			node,
			depth,
			track,
			priority,
			max_delay,
			group,
			start_frame,
			start_byte,
			end_frame,
		})
	}

	fn encode_msg(&self, w: &mut Encoder<'_>, version: Version) -> Result<(), EncodeError> {
		match version {
			Version::Lite01 | Version::Lite02 => {
				return Err(EncodeError::Version);
			}
			_ => {}
		}

		self.broadcast.encode(w, version)?;
		super::instance::encode_pin(w, version, self.instance.as_ref())?;
		super::instance::encode_node(w, version, self.node, self.depth)?;
		w.string(&self.track)?;
		w.u8(super::priority::to_wire(self.priority));
		match version.has_fetch_max_delay() {
			true => w.varint_opt(
				self.max_delay
					.map(|delay| u64::try_from(delay.as_nanos().div_ceil(1_000_000)))
					.transpose()
					.map_err(|_| EncodeError::BoundsExceeded)?,
			)?,
			false if self.max_delay.is_some() => return Err(EncodeError::Version),
			false => {}
		}
		w.varint(self.group)?;

		if !version.has_frame_bounds() && (self.start_frame != 0 || self.end_frame.is_some()) {
			// The peer would serve the whole group, including frames the caller excluded.
			return Err(EncodeError::Version);
		}
		if !version.has_byte_start() && self.start_byte != 0 {
			// The peer would resend the frame from its start, misaligning the reader.
			return Err(EncodeError::Version);
		}
		if version.has_frame_bounds() {
			w.varint(self.start_frame)?;
		}
		if version.has_byte_start() {
			w.varint(self.start_byte)?;
		}
		if version.has_frame_bounds() {
			w.varint_opt(self.end_frame)?;
		}

		Ok(())
	}
}

#[cfg(test)]
mod test {
	use super::*;

	fn fetch_sample() -> Fetch<'static> {
		Fetch {
			instance: None,
			node: 0,
			depth: 0,
			broadcast: Path::from_static("room"),
			track: Cow::Borrowed("video"),
			priority: 3,
			max_delay: None,
			group: 7,
			start_frame: 0,
			start_byte: 0,
			end_frame: None,
		}
	}

	/// Lite-07 asks for the rest of a frame; an older peer cannot, so encoding refuses.
	#[test]
	fn fetch_start_byte_roundtrips_only_on_lite07() {
		let mut msg = fetch_sample();
		msg.start_frame = 2;
		msg.start_byte = 1 << 20;
		let got = fetch_roundtrip(Version::Lite07, &msg);
		assert_eq!((got.start_frame, got.start_byte), (2, 1 << 20));
		assert!(
			msg.encode_msg(
				&mut Encoder::new(&mut Vec::new(), Version::Lite06.into()),
				Version::Lite06
			)
			.is_err()
		);
		msg.start_byte = 0;
		assert_eq!(fetch_roundtrip(Version::Lite06, &msg).start_byte, 0);
	}

	#[test]
	fn fetch_max_delay_roundtrips_on_lite07() {
		let mut msg = fetch_sample();
		msg.max_delay = Some(std::time::Duration::ZERO);
		assert_eq!(
			fetch_roundtrip(Version::Lite07, &msg).max_delay,
			Some(std::time::Duration::ZERO)
		);

		msg.max_delay = Some(std::time::Duration::from_millis(250));
		assert_eq!(
			fetch_roundtrip(Version::Lite07, &msg).max_delay,
			Some(std::time::Duration::from_millis(250))
		);
		assert!(
			msg.encode_msg(
				&mut Encoder::new(&mut Vec::new(), Version::Lite06.into()),
				Version::Lite06
			)
			.is_err()
		);
	}

	fn fetch_roundtrip(version: Version, msg: &Fetch<'_>) -> Fetch<'static> {
		let mut buf = Vec::new();
		msg.encode_msg(&mut Encoder::new(&mut buf, version.into()), version)
			.unwrap();
		let mut slice = buf.as_slice();
		crate::coding::decode_from(&mut slice, version, Fetch::decode_msg)
			.unwrap()
			.into_owned()
	}

	/// Only lite-07 names the origin node; older versions decode as letting the
	/// publisher select.
	#[test]
	fn fetch_node_roundtrips_only_on_lite07() {
		let mut msg = fetch_sample();
		msg.node = 0x7a3f;
		assert_eq!(fetch_roundtrip(Version::Lite07, &msg).node, 0x7a3f);
		assert_eq!(fetch_roundtrip(Version::Lite06, &msg).node, 0);
	}

	#[test]
	fn fetch_roundtrips() {
		for version in [Version::Lite03, Version::Lite04, Version::Lite05] {
			let got = fetch_roundtrip(version, &fetch_sample());
			assert_eq!(got.broadcast, Path::from_static("room"));
			assert_eq!(got.track, "video");
			assert_eq!(got.priority, 3);
			assert_eq!(got.group, 7);
		}
	}

	#[test]
	fn fetch_frame_range_roundtrips() {
		let mut msg = fetch_sample();
		msg.start_frame = 2;
		msg.end_frame = Some(6);

		let got = fetch_roundtrip(Version::Lite06, &msg);
		assert_eq!((got.start_frame, got.end_frame), (2, Some(6)));
	}

	/// A range that ends before it starts can never be served.
	#[test]
	fn fetch_inverted_frame_range_is_invalid() {
		let mut msg = fetch_sample();
		msg.start_frame = 6;
		msg.end_frame = Some(2);

		let mut buf = Vec::new();
		msg.encode_msg(&mut Encoder::new(&mut buf, Version::Lite06.into()), Version::Lite06)
			.unwrap();
		assert!(matches!(
			crate::coding::decode_from(&mut buf.as_slice(), Version::Lite06, Fetch::decode_msg),
			Err(DecodeError::InvalidSubscribeLocation)
		));
	}

	/// Older peers serve the whole group, which is not what the caller asked for.
	#[test]
	fn fetch_frame_range_rejected_before_lite06() {
		let mut msg = fetch_sample();
		msg.start_frame = 2;

		let mut buf = Vec::new();
		assert!(
			msg.encode_msg(&mut Encoder::new(&mut buf, Version::Lite05.into()), Version::Lite05)
				.is_err()
		);
	}

	#[test]
	fn fetch_rejected_before_lite03() {
		let mut buf = Vec::new();
		assert!(
			fetch_sample()
				.encode_msg(&mut Encoder::new(&mut buf, Version::Lite02.into()), Version::Lite02)
				.is_err()
		);
	}
}
