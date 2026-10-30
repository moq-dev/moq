//! H.264 single-rendition Annex-B exporter.
//!
//! Subscribes to one H.264 rendition from a catalog-narrowed stream and emits
//! a raw Annex-B elementary stream. Suitable for piping into `ffmpeg`, decoder
//! fuzzers, or recording one codec to disk. There is no container framing
//! (timestamps are dropped).
//!
//! A source without a decoder description is already Annex-B and passes through unchanged.
//! With an avcC description, length prefixes become start codes and any out-of-band
//! parameter sets precede keyframes. avc3 samples may carry all parameter sets inline.

use std::task::Poll;

use bytes::Bytes;
use hang::catalog::{VideoCodecKind, VideoConfig};

use crate::catalog::Stream;
use crate::codec::annexb;

/// Single-rendition H.264 Annex-B exporter.
pub struct Export<S: Stream> {
	inner: annexb::Export<S, Codec>,
}

impl<S: Stream> Export<S> {
	/// Subscribe to `source` and emit an Annex-B H.264 byte stream.
	///
	/// `catalog` is expected to be narrowed to a single H.264 rendition by name (e.g.
	/// `consumer.select(select::Broadcast::default().video(select::Video::default().name("hd")))`).
	/// Renditions of other codecs are ignored; if multiple H.264 renditions appear
	/// in a snapshot, the first by BTreeMap order wins and a warning is logged.
	pub fn new(source: crate::Source, catalog: S) -> Self {
		Self {
			inner: annexb::Export::new(source, catalog),
		}
	}

	/// Set the max delay for the per-track source.
	///
	/// See [`Consumer`](crate::container::Consumer) for the per-track skip behavior.
	/// Defaults to
	/// [`std::time::Duration::ZERO`](std::time::Duration::ZERO) (skip aggressively).
	pub fn with_max_delay(mut self, max_delay: std::time::Duration) -> Self {
		self.inner = self.inner.with_max_delay(max_delay);
		self
	}

	/// Wait for the next Annex B access unit, or `None` once the export ends.
	pub async fn next(&mut self) -> crate::Result<Option<Bytes>> {
		kio::wait(|waiter| self.poll_next(waiter)).await
	}

	/// Poll for the next Annex B access unit, or `None` once the export ends.
	pub fn poll_next(&mut self, waiter: &kio::Waiter) -> Poll<crate::Result<Option<Bytes>>> {
		self.inner.poll_next(waiter)
	}
}

struct Codec;

impl annexb::Codec for Codec {
	const KIND: VideoCodecKind = VideoCodecKind::H264;

	fn convert(name: &str, config: &VideoConfig) -> crate::Result<Option<annexb::Convert>> {
		Ok(match config.description.as_ref().filter(|d| !d.is_empty()) {
			None => None,
			Some(avcc) => {
				let params = super::Avcc::parse(avcc)?;
				if matches!(&config.codec, hang::catalog::VideoCodec::H264(codec) if !codec.inline)
					&& (params.sps.is_empty() || params.pps.is_empty())
				{
					return Err(super::Error::MissingParamSets {
						name: name.to_string(),
						sps: params.sps.len(),
						pps: params.pps.len(),
					}
					.into());
				}
				let prefix = annexb::build_prefix(params.sps.iter().chain(params.pps.iter()));
				Some(annexb::Convert {
					length_size: params.length_size,
					keyframe_prefix: prefix,
				})
			}
		})
	}
}

#[cfg(test)]
mod tests {
	use std::task::Poll;

	use bytes::Bytes;
	use hang::catalog::{H264, VideoConfig};

	use super::*;
	use crate::catalog::Stream;
	use crate::catalog::hang::Catalog;

	/// One-shot Stream that yields a single catalog snapshot then closes.
	struct Once(Option<Catalog>);

	impl Stream for Once {
		type Ext = ();

		fn poll_next(&mut self, _: &kio::Waiter) -> Poll<crate::Result<Option<Catalog>>> {
			Poll::Ready(Ok(self.0.take()))
		}
	}

	/// Build an avc1-shaped catalog snapshot with the supplied avcC bytes.
	fn avc1_catalog(name: &str, avcc: Bytes) -> Catalog {
		let mut config = VideoConfig::new(H264 {
			profile: 0x42,
			constraints: 0,
			level: 0x1f,
			inline: false,
		});
		config.coded_width = Some(320);
		config.coded_height = Some(240);
		config.description = Some(avcc);
		config.container = hang::catalog::Container::Legacy;

		let mut catalog = Catalog::default();
		catalog.video.insert(name, config).expect("duplicate rendition");
		catalog
	}

	/// Build a minimal avcC carrying one SPS + one PPS.
	fn build_avcc(sps: &[u8], pps: &[u8]) -> Bytes {
		super::super::build_avcc(&[Bytes::copy_from_slice(sps)], &[Bytes::copy_from_slice(pps)]).unwrap()
	}

	/// Write a length-prefixed (4-byte) NAL frame onto a moq-net group via
	/// the Legacy wire codec.
	fn write_length_prefixed(group: &mut moq_net::group::Producer, timestamp_us: u64, nals: &[&[u8]]) {
		let mut payload = bytes::BytesMut::new();
		for nal in nals {
			payload.extend_from_slice(&(nal.len() as u32).to_be_bytes());
			payload.extend_from_slice(nal);
		}
		let frame = crate::container::Frame {
			timestamp: moq_net::Timestamp::from_micros(timestamp_us).unwrap(),
			payload: payload.freeze(),
			keyframe: false, // Legacy wire format drops this; Consumer reconstructs.
			duration: None,
		};
		<crate::catalog::hang::Container as crate::container::Container>::write(
			&crate::catalog::hang::Container::Legacy(crate::container::Kind::Data),
			group,
			&[frame],
		)
		.unwrap();
	}

	/// Regression: when source is avc1 (length-prefixed + out-of-band avcC),
	/// the exporter must inject SPS+PPS before every keyframe and convert
	/// length prefixes to start codes for every frame.
	#[tokio::test(start_paused = true)]
	async fn a_moved_rendition_uses_its_new_parameter_sets() {
		use crate::container::test_util::{Moving, marked_frame};
		let mut moving = Moving::new();
		let first = avc1_catalog(
			"main",
			build_avcc(crate::container::test_util::SPS, crate::container::test_util::PPS),
		);
		let second = avc1_catalog("main", build_avcc(Moving::MAIN_SPS, crate::container::test_util::PPS));
		let mut first = first.video.renditions["main"].clone();
		first.track = Some("a".into());
		moving
			.catalog
			.modify()
			.unwrap()
			.video
			.renditions
			.insert("main".into(), first.clone());
		let mut export = Export::new(moving.source().await, moving.catalog_stream().await);
		moving.a.write(marked_frame(0, 0xa1)).unwrap();
		let first_prefix = <Codec as annexb::Codec>::convert("main", &first)
			.unwrap()
			.unwrap()
			.keyframe_prefix;
		assert!(next(&mut export).await.starts_with(&first_prefix));
		let mut second = second.video.renditions["main"].clone();
		second.track = Some("b".into());
		let second_prefix = <Codec as annexb::Codec>::convert("main", &second)
			.unwrap()
			.unwrap()
			.keyframe_prefix;
		assert_ne!(first_prefix, second_prefix);
		moving
			.catalog
			.modify()
			.unwrap()
			.video
			.renditions
			.insert("main".into(), second);
		moving.b.write(marked_frame(30, 0xb1)).unwrap();
		let frame = next(&mut export).await;
		assert!(frame.starts_with(&second_prefix));
		assert!(!frame.starts_with(&first_prefix));
	}

	#[tokio::test(start_paused = true)]
	async fn avc1_export_injects_sps_pps_on_keyframes() {
		let sps: &[u8] = &[
			0x67, 0x42, 0xc0, 0x1f, 0xda, 0x01, 0x40, 0x16, 0xe9, 0xb8, 0x08, 0x08, 0x0a, 0x00, 0x00, 0x07, 0xd0, 0x00,
			0x01, 0xd4, 0xc0, 0x80,
		];
		let pps: &[u8] = &[0x68, 0xce, 0x3c, 0x80];
		let idr: &[u8] = &[0x65, 0x88, 0x84, 0x21];
		let p_slice: &[u8] = &[0x61, 0xe0, 0x12, 0x34];

		let avcc = build_avcc(sps, pps);
		let catalog = avc1_catalog("video.m4s", avcc);

		// Producer side: publish the broadcast with one length-prefixed video track.
		let broadcast = moq_net::broadcast::Info::new().produce();
		let track = broadcast
			.create_track("video.m4s", hang::container::track_info(hang::catalog::PRIORITY.video))
			.unwrap();

		// Group 0 (keyframe-starting group): one IDR frame.
		let mut g0 = track.create_group(moq_net::group::Info { sequence: 0 }).unwrap();
		write_length_prefixed(&mut g0, 0, &[idr]);
		g0.finish().unwrap();

		// Group 1 (next group): one P-slice. Consumer marks the first frame
		// of every group as keyframe by protocol invariant, so the exporter
		// MUST treat both group-starts as keyframes and inject SPS+PPS twice.
		let mut g1 = track.create_group(moq_net::group::Info { sequence: 1 }).unwrap();
		write_length_prefixed(&mut g1, 33_000, &[p_slice]);
		g1.finish().unwrap();
		track.finish().unwrap();

		// Consumer side: run the exporter.
		let consumer = broadcast.consume();
		// The whole track is written before the exporter runs, so it needs a budget
		// wide enough to read it: the default skips everything but the live edge.
		let mut export = Export::new(crate::source::announced(&consumer).await, Once(Some(catalog)))
			.with_max_delay(std::time::Duration::from_secs(30));

		let frame0 = export.next().await.unwrap().expect("first frame");
		let frame1 = export.next().await.unwrap().expect("second frame");
		assert!(export.next().await.unwrap().is_none(), "track ended");

		// Build the expected SPS+PPS prefix and assert it's prepended to both
		// frames (group boundaries become keyframes).
		let prefix =
			crate::codec::annexb::build_prefix([Bytes::copy_from_slice(sps), Bytes::copy_from_slice(pps)].iter());

		assert!(
			frame0.starts_with(&prefix),
			"frame 0 (group 0 start) must begin with SPS+PPS prefix"
		);
		assert_eq!(
			&frame0[prefix.len()..],
			&[0, 0, 0, 1, 0x65, 0x88, 0x84, 0x21],
			"frame 0 IDR must follow the prefix in Annex-B form"
		);
		assert!(
			frame1.starts_with(&prefix),
			"frame 1 (group 1 start) is the first frame of its group and is treated as a keyframe by Consumer protocol; must begin with SPS+PPS prefix"
		);
		assert_eq!(
			&frame1[prefix.len()..],
			&[0, 0, 0, 1, 0x61, 0xe0, 0x12, 0x34],
			"frame 1 P-slice must follow the prefix in Annex-B form"
		);
	}
	/// The next chunk, required to be ready without another frame arriving.
	async fn next<S: Stream>(export: &mut Export<S>) -> Bytes {
		moq_clock::timeout(std::time::Duration::from_millis(1), export.next())
			.await
			.expect("a frame is ready")
			.unwrap()
			.expect("the export continues")
	}

	/// A disabled rendition hands the stream to its enabled sibling. Re-enabled, it takes the
	/// stream back at its first keyframe past what was written, skipping the stale group a new
	/// subscription starts on.
	#[tokio::test(start_paused = true)]
	async fn follows_the_catalog_across_a_disable() {
		use crate::container::test_util::{Live, raw_frame};

		let mut live = Live::avc3();
		let primary = live.track.name().to_string();
		let config = live.catalog.snapshot().video.renditions[&primary].clone();
		let mut sibling = live.add_track(".sibling", |catalog, name| {
			catalog.modify().unwrap().video.renditions.insert(name, config);
		});
		assert!(primary.as_str() < sibling.name(), "the primary is first by name");
		let set_enabled = |live: &mut Live, enabled: bool| {
			live.catalog
				.modify()
				.unwrap()
				.video
				.renditions
				.get_mut(&primary)
				.unwrap()
				.enabled = enabled;
		};
		let mut export = Export::new(live.source().await, live.catalog_stream().await);

		live.track.write(raw_frame(0, b"primary-0", true)).unwrap();
		sibling.write(raw_frame(0, b"sibling-0", true)).unwrap();
		assert_eq!(next(&mut export).await.as_ref(), b"primary-0");

		set_enabled(&mut live, false);
		sibling.write(raw_frame(100_000, b"sibling-1", true)).unwrap();
		assert_eq!(next(&mut export).await.as_ref(), b"sibling-1");

		set_enabled(&mut live, true);
		assert!(
			moq_clock::timeout(std::time::Duration::from_millis(1), export.next())
				.await
				.is_err(),
			"the stale group the primary stopped on is not replayed"
		);
		live.track.write(raw_frame(200_000, b"primary-2", true)).unwrap();
		assert_eq!(next(&mut export).await.as_ref(), b"primary-2");
	}

	#[tokio::test(start_paused = true)]
	async fn in_band_export_accepts_empty_parameter_sets() {
		for in_band in [false, true] {
			let mut catalog = avc1_catalog("video", Bytes::from_static(&[1, 0x42, 0, 0x1f, 0xff, 0xe0, 0]));
			let config = catalog.video.renditions.get_mut("video").unwrap();
			let hang::catalog::VideoCodec::H264(codec) = &mut config.codec else {
				unreachable!()
			};
			codec.inline = in_band;
			let broadcast = moq_net::broadcast::Info::new().produce();
			let track = broadcast
				.create_track("video", hang::container::track_info(hang::catalog::PRIORITY.video))
				.unwrap();
			let mut group = track.create_group(moq_net::group::Info { sequence: 0 }).unwrap();
			let nals: &[&[u8]] = &[&[0x67, 0x42, 0, 0x1f], &[0x68, 0xce], &[0x65, 0x88]];
			write_length_prefixed(&mut group, 0, nals);
			group.finish().unwrap();
			track.finish().unwrap();
			let consumer = broadcast.consume();
			let mut exporter = Export::new(crate::source::announced(&consumer).await, Once(Some(catalog)));
			let result = exporter.next().await;
			if in_band {
				let expected: Vec<_> = nals
					.iter()
					.flat_map(|nal| [0, 0, 0, 1].into_iter().chain(nal.iter().copied()))
					.collect();
				assert_eq!(result.unwrap().unwrap().as_ref(), expected.as_slice());
				assert!(exporter.next().await.unwrap().is_none());
			} else {
				assert!(matches!(
					result,
					Err(crate::Error::H264(super::super::Error::MissingParamSets { .. }))
				));
			}
		}
	}
}
