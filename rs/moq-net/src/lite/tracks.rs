use std::borrow::Cow;

use crate::{
	Path,
	coding::{Decode, DecodeError, Decoder, Encode, EncodeError, Encoder},
};

use super::{Message, TrackInfo, Version, message::MAX_MESSAGE_SIZE, message::decode_size};

const TRACKS_ADD: u64 = 0x0;
const TRACKS_REMOVE: u64 = 0x1;
const TRACKS_FINISH: u64 = 0x2;
// The same event the announce stream marks its initial set with.
const SYNCED: u64 = 0x4;

/// Sent by the subscriber on a Tracks Stream (0x8) to list one broadcast's tracks.
///
/// Lite07+ only.
#[derive(Clone, Debug)]
pub struct Tracks<'a> {
	pub broadcast: Path<'a>,
	/// The publisher instance the subscriber expects; see [`crate::origin::Route::epoch`].
	pub epoch: Option<crate::Epoch>,
}

impl Message for Tracks<'_> {
	const CONTROL: Option<crate::stats::Control> = Some(crate::stats::Control::TrackInfo);
	fn decode_msg(r: &mut Decoder<'_>, version: Version) -> Result<Self, DecodeError> {
		if !version.has_tracks() {
			return Err(DecodeError::Version);
		}
		let broadcast = Path::decode(r, version)?;
		let epoch = super::epoch::decode_epoch(r, version)?;
		Ok(Self { broadcast, epoch })
	}

	fn encode_msg(&self, w: &mut Encoder<'_>, version: Version) -> Result<(), EncodeError> {
		if !version.has_tracks() {
			return Err(EncodeError::Version);
		}
		self.broadcast.encode(w, version)?;
		super::epoch::encode_epoch(w, version, self.epoch.as_ref())
	}
}

/// One event the publisher sends on a Tracks Stream, each a type, then a size-prefixed body.
///
/// Lite07+ only.
#[derive(Clone, Debug)]
pub enum TracksEvent<'a> {
	/// TRACKS_ADD: a listed track, with the TRACK_INFO a Track Stream would answer.
	Added { name: Cow<'a, str>, info: TrackInfo },
	/// TRACKS_REMOVE: a track named by an earlier TRACKS_ADD is no longer listed, unfinished.
	Removed { name: Cow<'a, str> },
	/// TRACKS_FINISH: a track named by an earlier TRACKS_ADD is no longer listed because it
	/// finished.
	Finished { name: Cow<'a, str> },
	/// SYNCED: every track this hop listed when the stream opened, and still lists, was sent.
	Synced,
}

impl Encode<Version> for TracksEvent<'_> {
	fn encode(&self, w: &mut Encoder<'_>, version: Version) -> Result<(), EncodeError> {
		w.control = Some(crate::stats::Control::TrackInfo);
		if !version.has_tracks() {
			return Err(EncodeError::Version);
		}
		w.varint(match self {
			Self::Added { .. } => TRACKS_ADD,
			Self::Removed { .. } => TRACKS_REMOVE,
			Self::Finished { .. } => TRACKS_FINISH,
			Self::Synced => SYNCED,
		})?;
		let prefix = w.prefix_varint();
		match self {
			Self::Added { name, info } => {
				w.string(name)?;
				info.encode(w, version)?;
			}
			Self::Removed { name } | Self::Finished { name } => w.string(name)?,
			Self::Synced => {}
		}
		if w.since(&prefix) > MAX_MESSAGE_SIZE {
			w.discard(prefix);
			return Err(EncodeError::TooLarge);
		}
		w.fill(prefix)
	}
}

impl Decode<Version> for TracksEvent<'_> {
	fn decode(r: &mut Decoder<'_>, version: Version) -> Result<Self, DecodeError> {
		r.control = Some(crate::stats::Control::TrackInfo);
		if !version.has_tracks() {
			return Err(DecodeError::Version);
		}
		let typ = r.varint()?;
		let size = decode_size(r, MAX_MESSAGE_SIZE)?;
		let mut body = r.sub(size)?;
		// The body is complete, so running short inside it is malformed, not a wait for more.
		let msg = (|| {
			Ok(match typ {
				TRACKS_ADD => Self::Added {
					name: Cow::Owned(body.string()?),
					info: TrackInfo::decode(&mut body, version)?,
				},
				TRACKS_REMOVE => Self::Removed {
					name: Cow::Owned(body.string()?),
				},
				TRACKS_FINISH => Self::Finished {
					name: Cow::Owned(body.string()?),
				},
				SYNCED => Self::Synced,
				// A new stream with no earlier builds to tolerate.
				_ => return Err(DecodeError::InvalidValue),
			})
		})()
		.map_err(DecodeError::complete)?;
		match body.is_empty() {
			true => Ok(msg),
			false => Err(DecodeError::Long),
		}
	}
}

#[cfg(test)]
mod test {
	use std::time::Duration;

	use super::*;
	use crate::Timescale;

	fn roundtrip(event: &TracksEvent<'_>) -> TracksEvent<'static> {
		let buf = event.encode_bytes(Version::Lite07).unwrap();
		TracksEvent::decode_slice(&buf, Version::Lite07).unwrap().0
	}

	#[test]
	fn events_roundtrip() {
		let info = TrackInfo {
			refresh: Some(4),
			priority: 3,
			max_age: Some(Duration::from_secs(2)),
			timescale: Some(Timescale::MICRO),
			compress: true,
		};
		let TracksEvent::Added { name, info: got } = roundtrip(&TracksEvent::Added {
			name: "video".into(),
			info,
		}) else {
			panic!("expected an add");
		};
		assert_eq!(name, "video");
		assert_eq!(got.refresh, Some(4));
		assert_eq!(got.priority, 3);
		assert_eq!(got.max_age, Some(Duration::from_secs(2)));
		assert_eq!(got.timescale, Some(Timescale::MICRO));
		assert!(got.compress);

		let TracksEvent::Finished { name } = roundtrip(&TracksEvent::Finished { name: "audio".into() }) else {
			panic!("expected TRACKS_FINISH");
		};
		assert_eq!(name, "audio");
		let TracksEvent::Removed { name } = roundtrip(&TracksEvent::Removed { name: "video".into() }) else {
			panic!("expected a removal");
		};
		assert_eq!(name, "video");
		assert!(matches!(roundtrip(&TracksEvent::Synced), TracksEvent::Synced));
	}

	#[test]
	fn synced_is_an_empty_type_four() {
		assert_eq!(
			TracksEvent::Synced.encode_bytes(Version::Lite07).unwrap(),
			&[0x04u8, 0x00][..]
		);
	}

	#[test]
	fn an_unknown_event_is_refused() {
		assert!(matches!(
			TracksEvent::decode_slice(&[0x03, 0x00], Version::Lite07),
			Err(DecodeError::InvalidValue)
		));
	}

	#[test]
	fn a_trailing_byte_is_refused() {
		assert!(matches!(
			TracksEvent::decode_slice(&[0x04, 0x01, 0x00], Version::Lite07),
			Err(DecodeError::Long)
		));
	}

	#[test]
	fn the_request_roundtrips() {
		let epoch: crate::Epoch = "01900000-0000-7000-8000-000000000001".parse().unwrap();
		let msg = Tracks {
			broadcast: Path::new("room/alice").to_owned(),
			epoch: Some(epoch.clone()),
		};
		let buf = msg.encode_bytes(Version::Lite07).unwrap();
		let (got, _) = Tracks::decode_slice(&buf, Version::Lite07).unwrap();
		assert_eq!(got.broadcast, Path::new("room/alice"));
		assert_eq!(got.epoch, Some(epoch));
	}

	#[test]
	fn older_versions_have_no_tracks_stream() {
		let msg = Tracks {
			broadcast: Path::new("room").to_owned(),
			epoch: None,
		};
		assert!(msg.encode_bytes(Version::Lite06).is_err());
		assert!(TracksEvent::Synced.encode_bytes(Version::Lite06).is_err());
	}
}
