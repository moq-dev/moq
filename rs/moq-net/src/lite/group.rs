use crate::coding::*;

use super::{Message, Version};

#[derive(Clone, Debug)]
pub struct Group {
	// The subscribe ID.
	pub subscribe: u64,

	// The group key
	pub key: u64,

	// The index of the first frame on this stream. 0 (the common case) means the stream
	// carries the group from its beginning; a higher value means the leading frames are
	// missing, either because the subscription started partway in or because the
	// publisher only holds the tail. Lite06+ only; older versions always start at 0.
	pub frame_start: u64,

	/// Whether the stream carries each frame's header without its payload: the metadata
	/// mode the subscription asked for when the stream opened. Lite07+ only.
	pub meta: bool,
}

impl Group {
	/// The varints the newest layout carries: Subscribe ID, Group Sequence and Frame Start.
	const FIELDS: usize = 3;
}

impl<'a> Message<'a> for Group {
	/// Every field at its longest varint, so a header still arriving holds few bytes at its
	/// [unidentified](crate::priority::RECV_UNIDENTIFIED) receive rank. Versions before
	/// lite-07 carry no more fields, in a shorter varint form.
	const MAX_SIZE: usize = Self::FIELDS * crate::coding::varint::MAX_LEN + 1;

	fn decode_msg(r: &mut Decoder<'a>, version: Version) -> Result<Self, DecodeError> {
		let subscribe = r.varint()?;
		let key = r.varint()?;
		let frame_start = match version.has_frame_bounds() {
			true => r.varint()?,
			false => 0,
		};
		let meta = match version.has_meta() {
			true => decode_bool(r)?,
			false => false,
		};

		Ok(Self {
			subscribe,
			key,
			frame_start,
			meta,
		})
	}

	fn encode_msg(&self, w: &mut Encoder<'_>, version: Version) -> Result<(), EncodeError> {
		w.varint(self.subscribe)?;
		w.varint(self.key)?;

		if version.has_frame_bounds() {
			w.varint(self.frame_start)?;
		} else if self.frame_start != 0 {
			// The peer would number the frames from 0 and silently misalign the group.
			return Err(EncodeError::Version);
		}

		match version.has_meta() {
			true => w.u8(self.meta.into()),
			// The peer would read the frame headers as payload.
			false if self.meta => return Err(EncodeError::Version),
			false => {}
		}

		Ok(())
	}
}

/// A one-byte boolean: 0 or 1, anything else malformed.
pub(super) fn decode_bool(r: &mut Decoder<'_>) -> Result<bool, DecodeError> {
	match r.u8()? {
		0 => Ok(false),
		1 => Ok(true),
		_ => Err(DecodeError::InvalidValue),
	}
}

#[cfg(test)]
mod test {
	use super::*;

	/// A partial group is self-describing: the stream says which frame it starts at.
	#[test]
	fn frame_start_roundtrips() {
		let msg = Group {
			subscribe: 1,
			key: 7,
			frame_start: 4,
			meta: false,
		};
		let mut buf = Vec::new();
		msg.encode_msg(&mut Encoder::new(&mut buf, Version::Lite06.into()), Version::Lite06)
			.unwrap();
		let got =
			crate::coding::decode_buf(&mut buf.as_slice(), Version::Lite06, |r, v| Group::decode_msg(r, v)).unwrap();
		assert_eq!((got.key, got.frame_start), (7, 4));
	}

	/// An older peer would number the frames from 0 and silently misalign the group.
	#[test]
	fn frame_start_rejected_before_lite06() {
		let msg = Group {
			subscribe: 1,
			key: 7,
			frame_start: 4,
			meta: false,
		};
		let mut buf = Vec::new();
		assert!(
			msg.encode_msg(&mut Encoder::new(&mut buf, Version::Lite05.into()), Version::Lite05)
				.is_err()
		);

		let whole = Group { frame_start: 0, ..msg };
		let mut buf = Vec::new();
		whole
			.encode_msg(&mut Encoder::new(&mut buf, Version::Lite05.into()), Version::Lite05)
			.unwrap();
		let got =
			crate::coding::decode_buf(&mut buf.as_slice(), Version::Lite05, |r, v| Group::decode_msg(r, v)).unwrap();
		assert_eq!(got.frame_start, 0);
	}

	/// The size cap admits the longest header every version can carry: each field at its
	/// form's largest value, whose minimal encoding is already that form's longest. Lite-07
	/// fills the cap exactly, and one byte more is refused at the length prefix.
	#[test]
	fn the_longest_header_reads_back_under_the_cap() {
		let versions = [
			Version::Lite01,
			Version::Lite02,
			Version::Lite03,
			Version::Lite04,
			Version::Lite05,
			Version::Lite06,
			Version::Lite07,
		];
		for version in versions {
			let (max, longest) = match Form::from(version) {
				Form::Quic => (crate::coding::varint::MAX_QUIC, 8),
				Form::LeadingOnes => (u64::MAX, crate::coding::varint::MAX_LEN),
			};
			let fields = if version.has_frame_bounds() { 3 } else { 2 };
			let msg = Group {
				subscribe: max,
				key: max,
				frame_start: if version.has_frame_bounds() { max } else { 0 },
				meta: version.has_meta(),
			};

			let mut body = Vec::new();
			msg.encode_msg(&mut Encoder::new(&mut body, version.into()), version)
				.unwrap();
			let flags = usize::from(version.has_meta());
			assert_eq!(body.len(), fields * longest + flags, "{version:?}");
			assert!(body.len() <= <Group as Message>::MAX_SIZE, "{version:?}");

			let mut wire = Vec::new();
			msg.encode(&mut Encoder::new(&mut wire, version.into()), version)
				.unwrap();
			let (got, used) = Group::decode_slice(&wire, version).unwrap();
			assert_eq!(used, wire.len(), "{version:?}");
			assert_eq!(
				(got.subscribe, got.key, got.frame_start, got.meta),
				(msg.subscribe, msg.key, msg.frame_start, msg.meta),
				"{version:?}"
			);
		}

		assert_eq!(<Group as Message>::MAX_SIZE, 28);
		let over = <Group as Message>::MAX_SIZE + 1;
		let mut wire = vec![over as u8];
		wire.resize(1 + over, 0);
		assert!(matches!(
			Group::decode_slice(&wire, Version::Lite07),
			Err(DecodeError::MessageTooLarge { size: 29, max: 28 })
		));
	}

	/// Each stream marks its own mode on lite-07, and an older peer cannot carry it.
	#[test]
	fn meta_marks_the_stream_on_lite07_only() {
		let msg = Group {
			subscribe: 1,
			key: 7,
			frame_start: 0,
			meta: true,
		};
		let mut buf = Vec::new();
		msg.encode_msg(&mut Encoder::new(&mut buf, Version::Lite07.into()), Version::Lite07)
			.unwrap();
		assert_eq!(buf.last(), Some(&1));
		let got =
			crate::coding::decode_buf(&mut buf.as_slice(), Version::Lite07, |r, v| Group::decode_msg(r, v)).unwrap();
		assert!(got.meta);

		// Any other byte is malformed.
		*buf.last_mut().unwrap() = 2;
		assert!(
			crate::coding::decode_buf(&mut buf.as_slice(), Version::Lite07, |r, v| Group::decode_msg(r, v)).is_err()
		);

		let mut buf = Vec::new();
		assert!(
			msg.encode_msg(&mut Encoder::new(&mut buf, Version::Lite06.into()), Version::Lite06)
				.is_err()
		);
	}
}
