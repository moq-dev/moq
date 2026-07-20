//! Shared scaffolding for exporter tests.

use bytes::{Bytes, BytesMut};
use hang::catalog::{AudioConfig, Container, H264, VideoConfig};
use moq_net::Timestamp;

/// A live single-track Legacy broadcast. All producers stay open, so an
/// exporter sees a stream that has not ended.
pub(crate) struct Live {
	pub(crate) track: crate::container::Producer<crate::catalog::hang::Container>,
	pub(crate) catalog: crate::catalog::Producer,
	consumer: moq_net::broadcast::Consumer,
	_broadcast: moq_net::broadcast::Producer,
}

impl Live {
	/// One track named `name`, with `insert` registering its catalog rendition.
	pub(crate) fn new(name: &str, insert: impl FnOnce(&mut crate::catalog::Producer, String)) -> Self {
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let consumer = broadcast.consume();
		let mut catalog = crate::catalog::Producer::new(&mut broadcast).unwrap();
		let track = broadcast
			.create_track(broadcast.unique_name(name), hang::container::track_info())
			.unwrap();
		insert(&mut catalog, track.name().to_string());
		Self {
			track: crate::container::Producer::new(track, crate::catalog::hang::Container::Legacy),
			catalog,
			consumer,
			_broadcast: broadcast,
		}
	}

	/// One Avc3-shape H.264 rendition (320x240 at 30 fps).
	pub(crate) fn avc3() -> Self {
		Self::new(".avc3", |catalog, name| {
			let mut config = VideoConfig::new(H264 {
				profile: 0x42,
				constraints: 0xc0,
				level: 0x1f,
				inline: true,
			});
			config.coded_width = Some(320);
			config.coded_height = Some(240);
			config.framerate = Some(30.0);
			config.container = Container::Legacy;
			catalog.lock().video.renditions.insert(name, config);
		})
	}

	/// One Legacy audio rendition.
	pub(crate) fn audio(mut config: AudioConfig) -> Self {
		config.container = Container::Legacy;
		Self::new(".audio", |catalog, name| {
			catalog.lock().audio.renditions.insert(name, config);
		})
	}

	pub(crate) fn source(&self) -> crate::Source {
		crate::source::announced(&self.consumer)
	}

	pub(crate) async fn catalog_stream(&self) -> crate::catalog::Consumer {
		crate::catalog::Consumer::<()>::new(&self.consumer, crate::catalog::CatalogFormat::Hang)
			.await
			.expect("catalog consumer")
	}
}

/// H.264 NALs used by [`video_frame`], exposed for tests that assert on them.
pub(crate) const SPS: &[u8] = &[0x67, 0x42, 0xc0, 0x1f, 0xde, 0xad, 0xbe, 0xef];
pub(crate) const PPS: &[u8] = &[0x68, 0xce, 0x3c, 0x80];
pub(crate) const IDR: &[u8] = &[0x65, 0x88, 0x84, 0x21, 0x00, 0x11, 0x22, 0x33];
const DELTA: &[u8] = &[0x41, 0x9a, 0x00, 0x01];

/// One Annex-B H.264 frame with no duration: SPS + PPS + IDR for a keyframe,
/// otherwise a single delta slice.
pub(crate) fn video_frame(timestamp_us: u64, keyframe: bool) -> crate::container::Frame {
	let nals: &[&[u8]] = if keyframe { &[SPS, PPS, IDR] } else { &[DELTA] };
	let mut payload = BytesMut::new();
	for nal in nals {
		payload.extend_from_slice(&[0, 0, 0, 1]);
		payload.extend_from_slice(nal);
	}
	crate::container::Frame {
		timestamp: Timestamp::from_micros(timestamp_us).unwrap(),
		payload: payload.freeze(),
		keyframe,
		duration: None,
	}
}

/// One frame with a fixed payload and no duration.
pub(crate) fn raw_frame(timestamp_us: u64, payload: &'static [u8], keyframe: bool) -> crate::container::Frame {
	crate::container::Frame {
		timestamp: Timestamp::from_micros(timestamp_us).unwrap(),
		payload: Bytes::from_static(payload),
		keyframe,
		duration: None,
	}
}
