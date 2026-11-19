use crate::coding::*;

use super::{Message, Version};

#[derive(Clone, Debug)]
pub struct Group {
	// The subscribe ID.
	pub subscribe: u64,

	// The group key
	pub key: u64,

	/// Lite-07: the start of the group before this one on the track, or `None` when the
	/// publisher does not know it. No group starts strictly between the two, so the starts
	/// between are skipped. A receiver refuses one at or above `key` as a malformed track.
	pub previous: Option<u64>,

	// The index of the first frame on this stream. 0 (the common case) means the stream
	// carries the group from its beginning; a higher value means the leading frames are
	// missing, because the subscription started partway in. Lite-06 only; other versions
	// always start at 0.
	pub frame_start: u64,
}

impl Group {
	/// The varints the longest layout carries: Subscribe ID, Group Start, and lite-07's
	/// Previous Start or lite-06's Frame Start.
	const FIELDS: usize = 3;

	/// The starts this header passes over as skipped; see [`passed_over`].
	pub(crate) fn passed_over(&self) -> std::ops::Range<u64> {
		passed_over(self.previous, self.key)
	}
}

/// The starts a GROUP for `start` passes over as skipped when its previous group starts at
/// `previous`: those strictly between. Empty when the previous group is unknown or just below.
pub(crate) fn passed_over(previous: Option<u64>, start: u64) -> std::ops::Range<u64> {
	match previous {
		Some(previous) => previous.saturating_add(1).min(start)..start,
		None => start..start,
	}
}

impl<'a> Message<'a> for Group {
	/// Every field at its longest varint, so a header still arriving holds few bytes at its
	/// [unidentified](crate::priority::RECV_UNIDENTIFIED) receive rank. Versions before
	/// lite-07 carry fewer fields, in a shorter varint form.
	const MAX_SIZE: usize = Self::FIELDS * crate::coding::varint::MAX_LEN;

	fn decode_msg(r: &mut Decoder<'a>, version: Version) -> Result<Self, DecodeError> {
		let subscribe = r.varint()?;
		let key = r.varint()?;
		// Plus one, so 0 says unknown.
		let previous = match version.has_group_end() {
			true => r.varint()?.checked_sub(1),
			false => None,
		};
		let frame_start = match version.has_group_frame_start() {
			true => r.varint()?,
			false => 0,
		};

		Ok(Self {
			subscribe,
			key,
			previous,
			frame_start,
		})
	}

	fn encode_msg(&self, w: &mut Encoder<'_>, version: Version) -> Result<(), EncodeError> {
		w.varint(self.subscribe)?;
		w.varint(self.key)?;
		if version.has_group_end() {
			let previous = match self.previous {
				Some(previous) => previous.checked_add(1).ok_or(EncodeError::InvalidState)?,
				None => 0,
			};
			w.varint(previous)?;
		}

		if version.has_group_frame_start() {
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
			key: 7,
			previous: None,
			frame_start: 4,
		};
		let mut buf = Vec::new();
		msg.encode_msg(&mut Encoder::new(&mut buf, Version::Lite06.into()), Version::Lite06)
			.unwrap();
		let got =
			crate::coding::decode_buf(&mut buf.as_slice(), Version::Lite06, |r, v| Group::decode_msg(r, v)).unwrap();
		assert_eq!((got.key, got.frame_start), (7, 4));
	}

	/// A peer of another version would number the frames from 0 and silently misalign the group.
	#[test]
	fn frame_start_rejected_off_lite06() {
		let msg = Group {
			subscribe: 1,
			key: 7,
			previous: None,
			frame_start: 4,
		};
		for version in [Version::Lite05, Version::Lite07] {
			let mut buf = Vec::new();
			assert!(
				msg.encode_msg(&mut Encoder::new(&mut buf, version.into()), version)
					.is_err(),
				"{version:?}"
			);
		}

		let whole = Group { frame_start: 0, ..msg };
		let mut buf = Vec::new();
		whole
			.encode_msg(&mut Encoder::new(&mut buf, Version::Lite05.into()), Version::Lite05)
			.unwrap();
		let got =
			crate::coding::decode_buf(&mut buf.as_slice(), Version::Lite05, |r, v| Group::decode_msg(r, v)).unwrap();
		assert_eq!(got.frame_start, 0);
	}

	/// Lite-07 carries the previous start plus one, 0 when unknown, and passes over the starts
	/// strictly between; earlier versions carry nothing, so the previous start reads unknown.
	#[test]
	fn previous_start_is_plus_one_on_lite07_only() {
		let encode = |previous, version: Version| {
			let msg = Group {
				subscribe: 1,
				key: 7,
				previous,
				frame_start: 0,
			};
			let mut buf = Vec::new();
			msg.encode_msg(&mut Encoder::new(&mut buf, version.into()), version)
				.unwrap();
			buf
		};
		assert_eq!(encode(Some(4), Version::Lite07), [1, 7, 5]);
		assert_eq!(encode(None, Version::Lite07), [1, 7, 0]);
		assert_eq!(encode(Some(4), Version::Lite06), [1, 7, 0]);

		let decode = |wire: &[u8], version| {
			crate::coding::decode_buf(&mut &wire[..], version, |r, v| Group::decode_msg(r, v)).unwrap()
		};
		let got = decode(&[1, 7, 5], Version::Lite07);
		assert_eq!((got.previous, got.passed_over()), (Some(4), 5..7));
		let got = decode(&[1, 7, 7], Version::Lite07);
		assert_eq!((got.previous, got.passed_over()), (Some(6), 7..7));
		let got = decode(&[1, 7, 0], Version::Lite07);
		assert_eq!((got.previous, got.passed_over()), (None, 7..7));
		assert_eq!(decode(&[1, 7, 0], Version::Lite06).previous, None);

		// One at or above the group's own start passes over nothing: the receiver refuses it.
		let got = decode(&[1, 7, 8], Version::Lite07);
		assert_eq!((got.previous, got.passed_over()), (Some(7), 7..7));
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
			let fields = 2 + usize::from(version.has_group_frame_start()) + usize::from(version.has_group_end());
			let msg = Group {
				subscribe: max,
				key: max,
				previous: version.has_group_end().then_some(max - 1),
				frame_start: if version.has_group_frame_start() { max } else { 0 },
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
				(got.subscribe, got.key, got.previous, got.frame_start),
				(msg.subscribe, msg.key, msg.previous, msg.frame_start),
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
