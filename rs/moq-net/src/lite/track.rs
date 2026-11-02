use std::{borrow::Cow, time::Duration};

use crate::{
	Path, Timescale,
	coding::{Decode, DecodeError, Decoder, Encode, EncodeError, Encoder},
};

use super::{Message, Version};

// Older JS readers use safe integer milliseconds; larger legacy ages mean no limit.
const LEGACY_UNLIMITED: u64 = (1u64 << 53) - 1;

/// Sent by the subscriber on a Track Stream (0x6) to request a track's immutable
/// publisher properties, without subscribing or fetching.
///
/// Lite05+ only.
#[derive(Clone, Debug)]
pub struct Track<'a> {
	pub broadcast: Path,
	/// The publisher instance the subscriber expects; see [`crate::origin::Route::epoch`].
	/// Lite07+ only.
	pub epoch: Option<crate::Epoch>,
	pub track: Cow<'a, str>,
}

impl Track<'_> {
	/// Copy the borrowed track name, so the request outlives the bytes it came from.
	pub fn into_owned(self) -> Track<'static> {
		Track {
			broadcast: self.broadcast,
			epoch: self.epoch,
			track: Cow::Owned(self.track.into_owned()),
		}
	}
}

impl<'a> Message<'a> for Track<'a> {
	const CONTROL: Option<crate::stats::Message> = Some(crate::stats::Message::TrackInfo);
	fn decode_msg(r: &mut Decoder<'a>, version: Version) -> Result<Self, DecodeError> {
		if !version.has_track_stream() {
			return Err(DecodeError::Version);
		}

		let broadcast = Path::decode(r, version)?;
		let epoch = super::epoch::decode_epoch(r, version)?;
		let track = Cow::Borrowed(r.str()?);

		Ok(Self {
			broadcast,
			epoch,
			track,
		})
	}

	fn encode_msg(&self, w: &mut Encoder<'_>, version: Version) -> Result<(), EncodeError> {
		if !version.has_track_stream() {
			return Err(EncodeError::Version);
		}

		self.broadcast.encode(w, version)?;
		super::epoch::encode_epoch(w, version, self.epoch.as_ref())?;
		w.string(&self.track)?;
		Ok(())
	}
}

/// The publisher's sole reply on a Track Stream, carrying the track's immutable
/// properties. Every field is fixed for the lifetime of the track, so a subscriber
/// fetches this once and reuses it across every SUBSCRIBE and FETCH.
///
/// Lite05+ only.
#[derive(Clone, Debug)]
pub struct TrackInfo {
	/// Refresh byte ratio, advertised on lite-07 only.
	pub refresh: Option<u32>,
	/// The publisher's tie-break priority for this track.
	pub priority: i8,
	/// Publisher Max Age: an upper bound on how long the publisher caches a
	/// non-latest group past the arrival of a newer one. Encoded as milliseconds.
	pub max_age: Option<Duration>,
	/// Per-frame timestamp scale (units per second), or `None` for an untimed track.
	/// Lite07 encodes `None` as zero; Lite05/06 have no untimed encoding, so `None`
	/// goes out as milliseconds (see [`Version::wire_timescale`]).
	pub timescale: Option<Timescale>,
	/// Whether FRAME payloads and datagrams are DEFLATE compressed per group, the only codec
	/// the wire's `Compression` id names. Lite07+ only: an earlier peer is served the frames
	/// inflated, so it is never set.
	pub compress: bool,
}

impl TrackInfo {
	/// The TRACK_INFO a publisher serves for `info` on `version`.
	pub(crate) fn serve(info: &crate::track::Info, version: Version) -> Self {
		Self {
			refresh: info.refresh,
			priority: info.priority,
			max_age: info.max_age,
			timescale: info.timescale,
			// An older peer is served the frames inflated.
			compress: info.compress && version.has_compression(),
		}
	}
}

impl From<TrackInfo> for crate::track::Info {
	fn from(info: TrackInfo) -> Self {
		Self::default()
			.with_timescale(info.timescale)
			.with_max_age(info.max_age)
			.with_priority(info.priority)
			.with_refresh(info.refresh)
			.with_compress(info.compress)
	}
}

/// TRACK_INFO `Compression`: no codec.
const COMPRESSION_NONE: u64 = 0;
/// TRACK_INFO `Compression`: per-group DEFLATE (draft-lcurley-moq-flate).
const COMPRESSION_DEFLATE: u64 = 1;

/// What reading a track's groups and datagrams needs from its TRACK_INFO and, with the
/// `ietf` extension, TRACK_PROPERTIES.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Framing {
	/// The units of every frame Timestamp on the wire, or `None` when frames carry none.
	pub timescale: Option<Timescale>,
	/// The draft the track's moq-transport publisher spoke, which serialized its objects'
	/// properties, or `None` for a moq-lite one, which carries no moq-transport state.
	pub ietf: Option<crate::ietf::Version>,
	/// The track is compressed, so a datagram's payload follows its inflated size.
	pub compressed: bool,
}

impl Framing {
	/// How `version` frames a track accepted as `info`. Lite05/06 stamp every frame, an
	/// untimed track's with its send time in milliseconds.
	pub fn new(info: &crate::track::Info, version: Version) -> Self {
		Self {
			timescale: version.wire_timescale(info.timescale),
			ietf: info.ietf(),
			compressed: info.compress,
		}
	}
}

impl<'a> Message<'a> for TrackInfo {
	const CONTROL: Option<crate::stats::Message> = Some(crate::stats::Message::TrackInfo);
	fn decode_msg(r: &mut Decoder<'a>, version: Version) -> Result<Self, DecodeError> {
		if !version.has_track_stream() {
			return Err(DecodeError::Version);
		}

		let priority = super::priority::from_wire(r.u8()?);
		super::subscribe::skip_group_order(r, version)?;
		let encoded = r.varint()?;
		let max_age = match version {
			Version::Lite05 | Version::Lite06 => (encoded < LEGACY_UNLIMITED).then(|| Duration::from_millis(encoded)),
			_ => encoded.checked_sub(1).map(Duration::from_millis),
		};
		let timescale = match r.varint()? {
			// Lite05/06 have no untimed encoding, so zero is malformed there.
			0 if !matches!(version, Version::Lite05 | Version::Lite06) => None,
			scale => Some(Timescale::new(scale).map_err(|_| DecodeError::InvalidValue)?),
		};

		let refresh = match version.has_refresh() {
			true => r
				.varint()?
				.checked_sub(1)
				.map(u32::try_from)
				.transpose()
				.map_err(|_| DecodeError::InvalidValue)?,
			false => None,
		};
		let compress = match version.has_compression() {
			true => match r.varint()? {
				COMPRESSION_NONE => false,
				COMPRESSION_DEFLATE => true,
				_ => return Err(DecodeError::InvalidValue),
			},
			false => false,
		};

		Ok(Self {
			priority,
			refresh,
			max_age,
			timescale,
			compress,
		})
	}

	fn encode_msg(&self, w: &mut Encoder<'_>, version: Version) -> Result<(), EncodeError> {
		if !version.has_track_stream() {
			return Err(EncodeError::Version);
		}

		w.u8(super::priority::to_wire(self.priority));
		super::subscribe::pad_group_order(w, version)?;
		let encoded = match (version, self.max_age) {
			(Version::Lite05 | Version::Lite06, None) => LEGACY_UNLIMITED,
			(Version::Lite05 | Version::Lite06, Some(age)) => age.as_millis().min(u128::from(LEGACY_UNLIMITED)) as u64,
			(_, None) => 0,
			(_, Some(age)) => u64::try_from(age.as_millis() + 1).map_err(|_| EncodeError::BoundsExceeded)?,
		};
		w.varint(encoded)?;
		w.varint(version.wire_timescale(self.timescale).map_or(0, u64::from))?;
		if version.has_refresh() {
			w.varint(self.refresh.map_or(0, |ratio| u64::from(ratio) + 1))?;
		}
		if version.has_compression() {
			w.varint(match self.compress {
				true => COMPRESSION_DEFLATE,
				false => COMPRESSION_NONE,
			})?;
		} else if self.compress {
			// The frames would arrive compressed with nothing saying so.
			return Err(EncodeError::Version);
		}
		Ok(())
	}
}

#[cfg(test)]
mod test {
	use super::*;

	fn info_sample() -> TrackInfo {
		TrackInfo {
			refresh: None,
			priority: 7,
			max_age: Some(Duration::from_millis(2000)),
			timescale: Some(Timescale::MICRO),
			compress: false,
		}
	}

	fn info_roundtrip(version: Version, info: &TrackInfo) -> TrackInfo {
		let mut buf = Vec::new();
		info.encode_msg(&mut Encoder::new(&mut buf, version.into()), version)
			.unwrap();
		let mut slice = buf.as_slice();
		crate::coding::decode_buf(&mut slice, version, |r, v| TrackInfo::decode_msg(r, v)).unwrap()
	}

	#[test]
	fn refresh_is_lite07_only() {
		for version in [Version::Lite05, Version::Lite06, Version::Lite07] {
			for ratio in [None, Some(0), Some(1), Some(u32::MAX)] {
				let info = TrackInfo {
					refresh: ratio,
					..info_sample()
				};
				let expected = if version.has_refresh() { ratio } else { None };
				assert_eq!(info_roundtrip(version, &info).refresh, expected);
			}
		}
	}

	#[test]
	fn optional_max_age_roundtrips() {
		for version in [Version::Lite05, Version::Lite06, Version::Lite07] {
			for max_age in [None, Some(Duration::ZERO), Some(Duration::from_secs(30))] {
				let info = TrackInfo {
					max_age,
					..info_sample()
				};
				assert_eq!(info_roundtrip(version, &info).max_age, max_age);
			}
		}
	}

	#[test]
	fn lite07_reserves_zero_for_none_and_offsets_finite_ages() {
		for (age, encoded) in [
			(None, 0),
			(Some(Duration::ZERO), 1),
			(Some(Duration::from_millis(10)), 11),
		] {
			let mut buf = Vec::new();
			TrackInfo {
				max_age: age,
				..info_sample()
			}
			.encode_msg(&mut Encoder::new(&mut buf, Version::Lite07.into()), Version::Lite07)
			.unwrap();
			assert_eq!(buf[1], encoded);
		}
	}

	#[test]
	fn legacy_unlimited_range_stays_safe_for_old_readers() {
		for version in [Version::Lite05, Version::Lite06] {
			for millis in [LEGACY_UNLIMITED - 1, LEGACY_UNLIMITED, 1 << 53, 1 << 60, (1 << 62) - 1] {
				let mut raw = Vec::new();
				let w = &mut Encoder::new(&mut raw, version.into());
				w.u8(0);
				super::super::subscribe::pad_group_order(w, version).unwrap();
				w.varint(millis).unwrap();
				w.varint(1000u64).unwrap();
				let decoded = TrackInfo::decode_msg(&mut Decoder::new(&raw, version.into()), version).unwrap();
				assert_eq!(
					decoded.max_age,
					(millis < LEGACY_UNLIMITED).then(|| Duration::from_millis(millis))
				);
				let info = TrackInfo {
					max_age: Some(Duration::from_millis(millis)),
					..info_sample()
				};
				let mut encoded = Vec::new();
				info.encode_msg(&mut Encoder::new(&mut encoded, version.into()), version)
					.unwrap();
				let old_reader = &mut Decoder::new(&encoded, version.into());
				old_reader.u8().unwrap();
				super::super::subscribe::skip_group_order(old_reader, version).unwrap();
				assert_eq!(old_reader.varint().unwrap(), millis.min(LEGACY_UNLIMITED));
			}
		}
	}

	#[test]
	fn track_info_roundtrips_on_lite05() {
		let got = info_roundtrip(Version::Lite05, &info_sample());
		assert_eq!(got.priority, 7);
		assert_eq!(got.max_age, Some(Duration::from_millis(2000)));
		assert_eq!(got.timescale, Some(Timescale::MICRO));
	}

	#[test]
	fn track_info_defaults_match_cross_language_wire_bytes() {
		// The default track is untimed, which lite-05 declares as milliseconds.
		let info = TrackInfo::serve(&crate::track::Info::default(), Version::Lite05);
		let mut buf = Vec::new();
		info.encode(&mut Encoder::new(&mut buf, Version::Lite05.into()), Version::Lite05)
			.unwrap();

		assert_eq!(
			buf,
			[
				0x0c, 0x80, 0x00, 0xc0, 0x1f, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x43, 0xe8
			]
		);
	}

	#[test]
	fn lite07_encodes_an_untimed_track_as_zero() {
		let info = TrackInfo {
			timescale: None,
			..info_sample()
		};
		let buf = info.encode_bytes(Version::Lite07).unwrap();
		assert_eq!(buf.last(), Some(&0));
		assert_eq!(info_roundtrip(Version::Lite07, &info).timescale, None);
	}

	#[test]
	fn lite06_declares_an_untimed_track_as_milliseconds() {
		for version in [Version::Lite05, Version::Lite06] {
			let info = TrackInfo {
				timescale: None,
				..info_sample()
			};
			assert_eq!(info_roundtrip(version, &info).timescale, Some(Timescale::MILLI));
		}
	}

	#[test]
	fn lite06_rejects_a_zero_timescale() {
		for version in [Version::Lite05, Version::Lite06] {
			let mut raw = Vec::new();
			let w = &mut Encoder::new(&mut raw, version.into());
			w.u8(0);
			super::super::subscribe::pad_group_order(w, version).unwrap();
			w.varint(0u64).unwrap();
			w.varint(0u64).unwrap();
			assert!(matches!(
				TrackInfo::decode_msg(&mut Decoder::new(&raw, version.into()), version),
				Err(DecodeError::InvalidValue)
			));
		}
	}

	#[test]
	fn lite07_carries_the_compression_codec() {
		for compress in [false, true] {
			let info = TrackInfo {
				compress,
				..info_sample()
			};
			let buf = info.encode_bytes(Version::Lite07).unwrap();
			assert_eq!(buf.last(), Some(&u8::from(compress)));
			assert_eq!(info_roundtrip(Version::Lite07, &info).compress, compress);
		}

		// The message body, without its length prefix, ends in the codec id.
		let mut buf = Vec::new();
		TrackInfo {
			compress: true,
			..info_sample()
		}
		.encode_msg(&mut Encoder::new(&mut buf, Version::Lite07.into()), Version::Lite07)
		.unwrap();
		// An unknown codec id, still a one-byte varint.
		*buf.last_mut().unwrap() = 2;
		let mut slice = buf.as_slice();
		assert!(matches!(
			crate::coding::decode_buf(&mut slice, Version::Lite07, |r, v| TrackInfo::decode_msg(r, v)),
			Err(DecodeError::InvalidValue)
		));
	}

	/// An older peer cannot be told, so a compressed track is never described to one.
	#[test]
	fn compression_needs_lite07() {
		let info = TrackInfo {
			compress: true,
			..info_sample()
		};
		for version in [Version::Lite05, Version::Lite06] {
			assert!(
				matches!(info.encode_bytes(version), Err(EncodeError::Version)),
				"{version}"
			);
			let plain = TrackInfo {
				compress: false,
				..info_sample()
			};
			assert!(!info_roundtrip(version, &plain).compress);
		}
	}

	#[test]
	fn track_info_errors_before_lite05() {
		let mut buf = Vec::new();
		assert!(
			info_sample()
				.encode_msg(&mut Encoder::new(&mut buf, Version::Lite04.into()), Version::Lite04)
				.is_err()
		);
	}

	#[test]
	fn track_info_roundtrips_varint_and_priority_bounds() {
		let info = TrackInfo {
			refresh: None,
			priority: i8::MIN,
			max_age: None,
			timescale: Some(Timescale::new((1u64 << 62) - 1).unwrap()),
			compress: false,
		};
		let got = info_roundtrip(Version::Lite05, &info);
		assert_eq!(got.priority, i8::MIN);
		let info = TrackInfo {
			priority: i8::MAX,
			..info
		};
		assert_eq!(info_roundtrip(Version::Lite05, &info).priority, i8::MAX);
		assert_eq!(got.max_age, info.max_age);
		assert_eq!(got.timescale, info.timescale);
	}

	#[test]
	fn track_info_encodes_sub_millisecond_max_age_as_zero() {
		let info = TrackInfo {
			refresh: None,
			priority: 0,
			max_age: Some(Duration::from_nanos(999_999)),
			timescale: Some(Timescale::MILLI),
			compress: false,
		};
		let got = info_roundtrip(Version::Lite05, &info);
		assert_eq!(got.max_age, Some(Duration::ZERO));
	}

	#[test]
	fn track_info_encode_rejects_max_age_past_varint() {
		// Lite-07 varints carry the whole u64, and the age goes on the wire plus one.
		let info = TrackInfo {
			refresh: None,
			priority: 7,
			max_age: Some(Duration::from_millis(u64::MAX)),
			timescale: Some(Timescale::MILLI),
			compress: false,
		};
		// The partial bytes are the Writer's to drop; see `a_failed_encode_leaves_no_partial_bytes`.
		assert!(matches!(
			info.encode_bytes(Version::Lite07),
			Err(EncodeError::BoundsExceeded)
		));
	}

	#[test]
	fn track_request_roundtrips_on_lite05() {
		let msg = Track {
			epoch: None,
			broadcast: Path::from_static("room"),
			track: Cow::Borrowed("video"),
		};
		let mut buf = Vec::new();
		msg.encode_msg(&mut Encoder::new(&mut buf, Version::Lite05.into()), Version::Lite05)
			.unwrap();
		let mut slice = buf.as_slice();
		let got = crate::coding::decode_from(&mut slice, Version::Lite05, Track::decode_msg).unwrap();
		assert_eq!(got.broadcast, Path::from_static("room"));
		assert_eq!(got.track, "video");
	}

	#[test]
	fn track_request_errors_before_lite05() {
		let msg = Track {
			epoch: None,
			broadcast: Path::from_static("room"),
			track: Cow::Borrowed("video"),
		};
		let mut buf = Vec::new();
		assert!(
			msg.encode_msg(&mut Encoder::new(&mut buf, Version::Lite04.into()), Version::Lite04)
				.is_err()
		);
	}
}
