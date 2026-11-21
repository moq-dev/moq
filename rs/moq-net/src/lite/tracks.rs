use std::borrow::Cow;

use crate::{
	Path,
	broadcast::Kind,
	coding::{Decode, DecodeError, Decoder, Encode, EncodeError, Encoder},
};

use super::{DatagramsInfo, Message, TrackInfo, Version, message::MAX_MESSAGE_SIZE, message::decode_size};

const TRACKS_ADD: u64 = 0x0;
const TRACKS_REMOVE: u64 = 0x1;
const TRACKS_FINISH: u64 = 0x2;
// The same event the announce stream marks its initial set with.
const SYNCED: u64 = 0x4;

const KIND_TRACK: u64 = 0x0;
const KIND_DATAGRAMS: u64 = 0x1;

/// Sent by the subscriber on a Tracks Stream (0x8) to list one broadcast's tracks.
///
/// Lite07+ only.
#[derive(Clone, Debug)]
pub struct Tracks {
	pub broadcast: Path,
	/// The instance the subscriber expects; see [`crate::origin::RouteInfo::instance`].
	pub instance: Option<crate::Instance>,
	/// The forwarding node whose claim the request follows, or `None` for whichever the
	/// publisher selects. Lite07+ only.
	pub node: Option<crate::route::NodeId>,
}

impl<'a> Message<'a> for Tracks {
	const CONTROL: Option<crate::stats::Message> = Some(crate::stats::Message::TrackInfo);
	fn decode_msg(r: &mut Decoder<'a>, version: Version) -> Result<Self, DecodeError> {
		if !version.has_tracks() {
			return Err(DecodeError::Version);
		}
		let broadcast = Path::decode(r, version)?;
		let instance = super::instance::decode_pin(r, version)?;
		let node = super::instance::decode_node(r, version)?;
		Ok(Self {
			broadcast,
			instance,
			node,
		})
	}

	fn encode_msg(&self, w: &mut Encoder<'_>, version: Version) -> Result<(), EncodeError> {
		if !version.has_tracks() {
			return Err(EncodeError::Version);
		}
		self.broadcast.encode(w, version)?;
		super::instance::encode_pin(w, version, self.instance.as_ref())?;
		super::instance::encode_node(w, version, self.node)
	}
}

/// A listed track's info on a Tracks Stream, whose variant is its kind: what the request
/// for that kind of track would answer.
#[derive(Clone, Debug)]
pub enum Listed {
	/// A track of groups, with the TRACK_INFO a Track Stream would answer.
	Track(TrackInfo),
	/// A datagram track, with the DATAGRAMS_INFO a Datagrams Stream would answer.
	Datagrams(DatagramsInfo),
}

impl Listed {
	/// The kind of track this info is for.
	pub fn kind(&self) -> Kind {
		match self {
			Self::Track(_) => Kind::Track,
			Self::Datagrams(_) => Kind::Datagrams,
		}
	}
}

fn encode_kind(w: &mut Encoder<'_>, kind: Kind) -> Result<(), EncodeError> {
	w.varint(match kind {
		Kind::Track => KIND_TRACK,
		Kind::Datagrams => KIND_DATAGRAMS,
	})
}

fn decode_kind(r: &mut Decoder<'_>) -> Result<Kind, DecodeError> {
	match r.varint()? {
		KIND_TRACK => Ok(Kind::Track),
		KIND_DATAGRAMS => Ok(Kind::Datagrams),
		_ => Err(DecodeError::InvalidValue),
	}
}

/// One event the publisher sends on a Tracks Stream, each a type, then a size-prefixed body.
/// A name is listed at most once per kind.
///
/// Lite07+ only.
#[derive(Clone, Debug)]
pub enum TracksEvent<'a> {
	/// TRACKS_ADD: a listed track, with its kind and info.
	Added { name: Cow<'a, str>, info: Listed },
	/// TRACKS_REMOVE: a track named by an earlier TRACKS_ADD is no longer listed, unfinished.
	Removed { kind: Kind, name: Cow<'a, str> },
	/// TRACKS_FINISH: a track named by an earlier TRACKS_ADD is no longer listed because it
	/// finished.
	Finished { kind: Kind, name: Cow<'a, str> },
	/// SYNCED: every track this hop listed when the stream opened, and still lists, was sent.
	Synced,
}

impl Encode<Version> for TracksEvent<'_> {
	fn encode(&self, w: &mut Encoder<'_>, version: Version) -> Result<(), EncodeError> {
		w.control = Some(crate::stats::Message::TrackInfo);
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
				encode_kind(w, info.kind())?;
				w.string(name)?;
				match info {
					Listed::Track(info) => info.encode(w, version)?,
					Listed::Datagrams(info) => info.encode(w, version)?,
				}
			}
			Self::Removed { kind, name } | Self::Finished { kind, name } => {
				encode_kind(w, *kind)?;
				w.string(name)?;
			}
			Self::Synced => {}
		}
		if w.since(&prefix) > MAX_MESSAGE_SIZE {
			w.discard(prefix);
			return Err(EncodeError::TooLarge);
		}
		w.fill(prefix)
	}
}

impl<'a> Decode<'a, Version> for TracksEvent<'a> {
	fn decode(r: &mut Decoder<'a>, version: Version) -> Result<Self, DecodeError> {
		r.control = Some(crate::stats::Message::TrackInfo);
		if !version.has_tracks() {
			return Err(DecodeError::Version);
		}
		let typ = r.varint()?;
		let size = decode_size(r, MAX_MESSAGE_SIZE)?;
		let mut body = r.sub(size)?;
		// The body is complete, so running short inside it is malformed, not a wait for more.
		let msg = (|| {
			Ok(match typ {
				TRACKS_ADD => {
					let kind = decode_kind(&mut body)?;
					let name = Cow::Borrowed(body.str()?);
					let info = match kind {
						Kind::Track => Listed::Track(TrackInfo::decode(&mut body, version)?),
						Kind::Datagrams => Listed::Datagrams(DatagramsInfo::decode(&mut body, version)?),
					};
					Self::Added { name, info }
				}
				TRACKS_REMOVE => Self::Removed {
					kind: decode_kind(&mut body)?,
					name: Cow::Borrowed(body.str()?),
				},
				TRACKS_FINISH => Self::Finished {
					kind: decode_kind(&mut body)?,
					name: Cow::Borrowed(body.str()?),
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
		match TracksEvent::decode_slice(&buf, Version::Lite07).unwrap().0 {
			TracksEvent::Added { name, info } => TracksEvent::Added {
				name: name.into_owned().into(),
				info,
			},
			TracksEvent::Removed { kind, name } => TracksEvent::Removed {
				kind,
				name: name.into_owned().into(),
			},
			TracksEvent::Finished { kind, name } => TracksEvent::Finished {
				kind,
				name: name.into_owned().into(),
			},
			TracksEvent::Synced => TracksEvent::Synced,
		}
	}

	#[test]
	fn events_roundtrip() {
		let info = TrackInfo {
			refresh: crate::track::Refresh::default().with_ratio(4),
			priority: 3,
			max_age: Some(Duration::from_secs(2)),
			timescale: Some(Timescale::MICRO),
			compression: crate::track::Compression::Deflate,
			tail: 4 << 20,
		};
		let TracksEvent::Added {
			name,
			info: Listed::Track(got),
		} = roundtrip(&TracksEvent::Added {
			name: "video".into(),
			info: Listed::Track(info),
		})
		else {
			panic!("expected an add");
		};
		assert_eq!(name, "video");
		assert_eq!(got.refresh, crate::track::Refresh::default().with_ratio(4));
		assert_eq!(got.priority, 3);
		assert_eq!(got.max_age, Some(Duration::from_secs(2)));
		assert_eq!(got.timescale, Some(Timescale::MICRO));
		assert_eq!(got.compression, crate::track::Compression::Deflate);

		let datagrams = DatagramsInfo {
			priority: -2,
			timescale: Some(Timescale::MILLI),
		};
		let TracksEvent::Added {
			name,
			info: Listed::Datagrams(got),
		} = roundtrip(&TracksEvent::Added {
			name: "video".into(),
			info: Listed::Datagrams(datagrams),
		})
		else {
			panic!("expected a datagram track");
		};
		assert_eq!(name, "video");
		assert_eq!(got, datagrams);

		for kind in [Kind::Track, Kind::Datagrams] {
			let TracksEvent::Finished { kind: got, name } = roundtrip(&TracksEvent::Finished {
				kind,
				name: "audio".into(),
			}) else {
				panic!("expected TRACKS_FINISH");
			};
			assert_eq!((got, &*name), (kind, "audio"));
			let TracksEvent::Removed { kind: got, name } = roundtrip(&TracksEvent::Removed {
				kind,
				name: "video".into(),
			}) else {
				panic!("expected a removal");
			};
			assert_eq!((got, &*name), (kind, "video"));
		}
		assert!(matches!(roundtrip(&TracksEvent::Synced), TracksEvent::Synced));
	}

	/// Each event names its kind ahead of the track's name: TRACKS_ADD of a datagram track
	/// is the kind, the name, then a DATAGRAMS_INFO with its own length.
	#[test]
	fn the_kind_leads_each_listed_track() {
		let added = TracksEvent::Added {
			name: "a".into(),
			info: Listed::Datagrams(DatagramsInfo {
				priority: 0,
				timescale: None,
			}),
		};
		// Type, length, kind 1, name "a", then DATAGRAMS_INFO: length 2, priority 0x80, no timescale.
		assert_eq!(
			added.encode_bytes(Version::Lite07).unwrap(),
			&[0x00u8, 0x06, 0x01, 0x01, b'a', 0x02, 0x80, 0x00][..]
		);
		let removed = TracksEvent::Removed {
			kind: Kind::Datagrams,
			name: "a".into(),
		};
		assert_eq!(
			removed.encode_bytes(Version::Lite07).unwrap(),
			&[0x01u8, 0x03, 0x01, 0x01, b'a'][..]
		);
	}

	/// A kind other than a track or a datagram track is malformed.
	#[test]
	fn an_unknown_kind_is_refused() {
		for typ in [0x00u8, 0x01, 0x02] {
			assert!(matches!(
				TracksEvent::decode_slice(&[typ, 0x03, 0x02, 0x01, b'a'], Version::Lite07),
				Err(DecodeError::InvalidValue)
			));
		}
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
		let instance: crate::Instance = "alice".parse().unwrap();
		let msg = Tracks {
			broadcast: Path::from_static("room/alice"),
			instance: Some(instance.clone()),
			node: Some(crate::route::NodeId {
				relay: 0x7a3f,
				start: 9,
				marked: false,
			}),
		};
		let buf = msg.encode_bytes(Version::Lite07).unwrap();
		let (got, _) = Tracks::decode_slice(&buf, Version::Lite07).unwrap();
		assert_eq!(got.broadcast, Path::from_static("room/alice"));
		assert_eq!(got.instance, Some(instance));
		assert_eq!(
			got.node,
			Some(crate::route::NodeId {
				relay: 0x7a3f,
				start: 9,
				marked: false,
			})
		);
	}

	#[test]
	fn older_versions_have_no_tracks_stream() {
		let msg = Tracks {
			broadcast: Path::from_static("room"),
			instance: None,
			node: None,
		};
		assert!(msg.encode_bytes(Version::Lite06).is_err());
		assert!(TracksEvent::Synced.encode_bytes(Version::Lite06).is_err());
	}
}
