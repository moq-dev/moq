use crate::coding::*;

use super::{Message, Version};

#[derive(Clone, Debug)]
pub struct Group {
	// The subscribe ID.
	pub subscribe: u64,

	// The group sequence number
	pub sequence: u64,

	// The index of the first frame on this stream. 0 (the common case) means the stream
	// carries the group from its beginning; a higher value means the leading frames are
	// missing, either because the subscription started partway in or because the
	// publisher only holds the tail. Lite06+ only; older versions always start at 0.
	pub frame_start: u64,
}

impl Group {
	/// The varints the newest layout carries: Subscribe ID, Group Sequence and Frame Start.
	const FIELDS: usize = 3;
}

impl<'a> Message<'a> for Group {
	/// Every field at its longest varint, so a header still arriving holds few bytes at its
	/// [unidentified](crate::priority::RECV_UNIDENTIFIED) receive rank. Versions before
	/// lite-07 carry no more fields, in a shorter varint form.
	const MAX_SIZE: usize = Self::FIELDS * crate::coding::varint::MAX_LEN;

	fn decode_msg(r: &mut Decoder<'a>, version: Version) -> Result<Self, DecodeError> {
		let subscribe = r.varint()?;
		let sequence = r.varint()?;
		let frame_start = match version.has_frame_bounds() {
			true => r.varint()?,
			false => 0,
		};

		Ok(Self {
			subscribe,
			sequence,
			frame_start,
		})
	}

	fn encode_msg(&self, w: &mut Encoder<'_>, version: Version) -> Result<(), EncodeError> {
		w.varint(self.subscribe)?;
		w.varint(self.sequence)?;

		if version.has_frame_bounds() {
			w.varint(self.frame_start)?;
		} else if self.frame_start != 0 {
			// The peer would number the frames from 0 and silently misalign the group.
			return Err(EncodeError::Version);
		}

		Ok(())
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
			sequence: 7,
			frame_start: 4,
		};
		let mut buf = Vec::new();
		msg.encode_msg(&mut Encoder::new(&mut buf, Version::Lite06.into()), Version::Lite06)
			.unwrap();
		let got =
			crate::coding::decode_buf(&mut buf.as_slice(), Version::Lite06, |r, v| Group::decode_msg(r, v)).unwrap();
		assert_eq!((got.sequence, got.frame_start), (7, 4));
	}

	/// An older peer would number the frames from 0 and silently misalign the group.
	#[test]
	fn frame_start_rejected_before_lite06() {
		let msg = Group {
			subscribe: 1,
			sequence: 7,
			frame_start: 4,
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
				sequence: max,
				frame_start: if version.has_frame_bounds() { max } else { 0 },
			};

			let mut body = Vec::new();
			msg.encode_msg(&mut Encoder::new(&mut body, version.into()), version)
				.unwrap();
			assert_eq!(body.len(), fields * longest, "{version:?}");
			assert!(body.len() <= <Group as Message>::MAX_SIZE, "{version:?}");

			let mut wire = Vec::new();
			msg.encode(&mut Encoder::new(&mut wire, version.into()), version)
				.unwrap();
			let (got, used) = Group::decode_slice(&wire, version).unwrap();
			assert_eq!(used, wire.len(), "{version:?}");
			assert_eq!(
				(got.subscribe, got.sequence, got.frame_start),
				(msg.subscribe, msg.sequence, msg.frame_start),
				"{version:?}"
			);
		}

		assert_eq!(<Group as Message>::MAX_SIZE, 27);
		let over = <Group as Message>::MAX_SIZE + 1;
		let mut wire = vec![over as u8];
		wire.resize(1 + over, 0);
		assert!(matches!(
			Group::decode_slice(&wire, Version::Lite07),
			Err(DecodeError::MessageTooLarge { size: 28, max: 27 })
		));
	}
}
