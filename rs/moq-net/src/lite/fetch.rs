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
	pub broadcast: Path<'a>,
	pub track: Cow<'a, str>,
	pub priority: u8,
	pub group: u64,
	/// Index of the first frame to return; 0 is the start of the group. Lite06+ only.
	pub start_frame: u64,
	/// Index of the last frame to return (inclusive), or `None` for through the end of
	/// the group. Lite06+ only.
	pub end_frame: Option<u64>,
}

impl Message for Fetch<'_> {
	fn decode_msg(r: &mut Decoder<'_>, version: Version) -> Result<Self, DecodeError> {
		match version {
			Version::Lite01 | Version::Lite02 => {
				return Err(DecodeError::Version);
			}
			_ => {}
		}

		let broadcast = Path::decode(r, version)?;
		let track = Cow::Owned(r.string()?);
		let priority = r.u8()?;
		let group = r.varint()?;

		let (start_frame, end_frame) = match version.has_frame_bounds() {
			true => (r.varint()?, r.varint_opt()?),
			false => (0, None),
		};
		// A range that ends before it starts can never be served.
		if end_frame.is_some_and(|end| end < start_frame) {
			return Err(DecodeError::InvalidSubscribeLocation);
		}

		Ok(Self {
			broadcast,
			track,
			priority,
			group,
			start_frame,
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
		w.string(&self.track)?;
		w.u8(self.priority);
		w.varint(self.group)?;

		if version.has_frame_bounds() {
			w.varint(self.start_frame)?;
			w.varint_opt(self.end_frame)?;
		} else if self.start_frame != 0 || self.end_frame.is_some() {
			// The peer would serve the whole group, including frames the caller excluded.
			return Err(EncodeError::Version);
		}

		Ok(())
	}
}

#[cfg(test)]
mod test {
	use super::*;

	fn fetch_sample() -> Fetch<'static> {
		Fetch {
			broadcast: Path::new("room").to_owned(),
			track: Cow::Borrowed("video"),
			priority: 3,
			group: 7,
			start_frame: 0,
			end_frame: None,
		}
	}

	fn fetch_roundtrip(version: Version, msg: &Fetch<'_>) -> Fetch<'static> {
		let mut buf = Vec::new();
		msg.encode_msg(&mut Encoder::new(&mut buf, version.into()), version)
			.unwrap();
		let mut slice = buf.as_slice();
		crate::coding::decode_buf(&mut slice, version, Fetch::decode_msg).unwrap()
	}

	#[test]
	fn fetch_roundtrips() {
		for version in [Version::Lite03, Version::Lite04, Version::Lite05] {
			let got = fetch_roundtrip(version, &fetch_sample());
			assert_eq!(got.broadcast, Path::new("room"));
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
			crate::coding::decode_buf(&mut buf.as_slice(), Version::Lite06, Fetch::decode_msg),
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
