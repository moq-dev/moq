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
	broadcast: moq_net::broadcast::Producer,
}

impl Live {
	/// One track named `name`, with `insert` registering its catalog rendition.
	pub(crate) fn new(name: &str, insert: impl FnOnce(&mut crate::catalog::Producer, String)) -> Self {
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let consumer = broadcast.consume();
		let mut catalog = crate::catalog::Producer::new(&mut broadcast, crate::catalog::Config::default()).unwrap();
		let track = broadcast
			.create_track(
				broadcast.unique_name(name),
				hang::container::track_info(hang::catalog::PRIORITY.video),
			)
			.unwrap();
		insert(&mut catalog, track.name().to_string());
		let kind = if catalog.modify().unwrap().video.renditions.contains_key(track.name()) {
			crate::container::Kind::Video
		} else {
			crate::container::Kind::Audio
		};
		let format = crate::catalog::hang::Container::Legacy(kind);
		Self {
			track: crate::container::Producer::new(track, format),
			catalog,
			consumer,
			broadcast,
		}
	}

	/// Add another track named `name` to the same broadcast, with `insert`
	/// registering its catalog rendition. Used to build an A/V broadcast.
	pub(crate) fn add_track(
		&mut self,
		name: &str,
		insert: impl FnOnce(&mut crate::catalog::Producer, String),
	) -> crate::container::Producer<crate::catalog::hang::Container> {
		let name = self.broadcast.unique_name(name);
		let track = self
			.broadcast
			.create_track(name, hang::container::track_info(hang::catalog::PRIORITY.audio))
			.unwrap();
		insert(&mut self.catalog, track.name().to_string());
		crate::container::Producer::new(
			track,
			crate::catalog::hang::Container::Legacy(crate::container::Kind::Data),
		)
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
			catalog.modify().unwrap().video.renditions.insert(name, config);
		})
	}

	/// One Legacy audio rendition.
	pub(crate) fn audio(mut config: AudioConfig) -> Self {
		config.container = Container::Legacy;
		Self::new(".audio", |catalog, name| {
			catalog.modify().unwrap().audio.renditions.insert(name, config);
		})
	}

	pub(crate) fn source(&self) -> crate::Source {
		crate::source::announced(&self.consumer)
	}

	pub(crate) async fn catalog_stream(&self) -> crate::catalog::Consumer {
		self.source()
			.catalog::<()>(crate::catalog::CatalogFormat::Hang)
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

/// A broadcast clock that began `ago` before now, so a first frame arriving now reads as late.
pub(crate) fn late_clock(ago: std::time::Duration) -> crate::Clock {
	crate::Clock::at(std::time::Instant::now() - ago, std::time::SystemTime::now() - ago).unwrap()
}

/// Every frame timestamp, in micros, that each media rendition in `catalog` published, by track.
///
/// Reads through each rendition's own container, so an fMP4 timestamp comes from the fragment's
/// `tfdt` rather than the wire header. The importer must have finished its tracks.
pub(crate) async fn published<E>(
	consumer: &moq_net::broadcast::Consumer,
	catalog: &hang::catalog::Catalog<E>,
) -> std::collections::BTreeMap<String, Vec<u128>> {
	let mut containers = Vec::new();
	for (name, config) in &catalog.video.renditions {
		containers.push((name.clone(), crate::catalog::hang::Container::try_from(config).unwrap()));
	}
	for (name, config) in &catalog.audio.renditions {
		containers.push((name.clone(), crate::catalog::hang::Container::try_from(config).unwrap()));
	}

	let mut out = std::collections::BTreeMap::new();
	for (name, container) in containers {
		let replay = moq_net::track::Subscription::default().with_max_age(std::time::Duration::from_secs(3600));
		let track = consumer.track(&name).unwrap().subscribe(replay).await.unwrap();
		let mut reader = crate::container::Consumer::new(track, container);
		let mut timestamps = Vec::new();
		while let Some(frame) = tokio::time::timeout(std::time::Duration::from_secs(5), reader.read())
			.await
			.expect("the importer finished its tracks")
			.unwrap()
		{
			timestamps.push(frame.timestamp.as_micros());
		}
		out.insert(name, timestamps);
	}
	out
}

/// The one offset every published timestamp moved by between a verbatim and a live import of the
/// same input, within a tick of rounding: one mapping for every track, so A/V sync and B-frame
/// order survive exactly.
pub(crate) fn common_offset(
	verbatim: &std::collections::BTreeMap<String, Vec<u128>>,
	live: &std::collections::BTreeMap<String, Vec<u128>>,
) -> i128 {
	assert_eq!(verbatim.keys().count(), live.keys().count(), "the same tracks publish");
	let mut offset = None;
	for (v, l) in verbatim.values().zip(live.values()) {
		assert_eq!(v.len(), l.len(), "the same frames publish");
		for (v, l) in v.iter().zip(l) {
			let delta = *l as i128 - *v as i128;
			let first = *offset.get_or_insert(delta);
			assert!(
				(delta - first).abs() <= 1_000,
				"every frame moves by one offset: {delta} vs {first}"
			);
		}
	}
	offset.expect("frames were published")
}
