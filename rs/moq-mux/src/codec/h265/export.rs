//! H.265 single-rendition Annex-B exporter.
//!
//! HEVC analogue of [`crate::codec::h264::Export`]. Sources without a description
//! are already Annex-B. With an hvcC description, length prefixes become start codes
//! and any out-of-band parameter sets precede keyframes. hev1 samples may carry all
//! parameter sets inline. Timestamps are dropped.

use std::task::Poll;

use bytes::Bytes;
use hang::catalog::{VideoCodecKind, VideoConfig};

use crate::catalog::Stream;
use crate::codec::annexb;

/// Single-rendition H.265 Annex-B exporter.
pub struct Export<S: Stream> {
	inner: annexb::Export<S, Codec>,
}

impl<S: Stream> Export<S> {
	/// Subscribe to `source` and emit an Annex-B H.265 byte stream.
	///
	/// `catalog` is expected to be narrowed to a single H.265 rendition. If
	/// multiple H.265 renditions appear in a snapshot, the first by BTreeMap
	/// order wins and a warning is logged.
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
	const KIND: VideoCodecKind = VideoCodecKind::H265;

	fn convert(name: &str, config: &VideoConfig) -> crate::Result<Option<annexb::Convert>> {
		Ok(match config.description.as_ref().filter(|d| !d.is_empty()) {
			None => None,
			Some(hvcc) => {
				let params = super::Hvcc::parse(hvcc)?;
				if matches!(&config.codec, hang::catalog::VideoCodec::H265(codec) if !codec.in_band)
					&& (params.vps.is_empty() || params.sps.is_empty() || params.pps.is_empty())
				{
					return Err(super::Error::MissingParamSets {
						name: name.to_string(),
						vps: params.vps.len(),
						sps: params.sps.len(),
						pps: params.pps.len(),
					}
					.into());
				}
				let prefix = annexb::build_prefix(params.vps.iter().chain(params.sps.iter()).chain(params.pps.iter()));
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

	use bytes::{Bytes, BytesMut};
	use hang::catalog::{H265, VideoConfig};

	use super::*;
	use crate::catalog::Stream;
	use crate::catalog::hang::Catalog;

	struct Once(Option<Catalog>);

	impl Stream for Once {
		type Ext = ();

		fn poll_next(&mut self, _: &kio::Waiter) -> Poll<crate::Result<Option<Catalog>>> {
			Poll::Ready(Ok(self.0.take()))
		}
	}

	fn hvc1_catalog(name: &str, hvcc: Bytes) -> Catalog {
		let mut config = VideoConfig::new(H265 {
			in_band: false,
			profile_space: 0,
			profile_idc: 1,
			profile_compatibility_flags: [0, 0, 0, 0],
			tier_flag: false,
			level_idc: 93,
			constraint_flags: [0, 0, 0, 0, 0, 0],
		});
		config.coded_width = Some(320);
		config.coded_height = Some(240);
		config.description = Some(hvcc);
		config.container = hang::catalog::Container::Legacy;

		let mut catalog = Catalog::default();
		catalog.video.insert(name, config).expect("duplicate rendition");
		catalog
	}

	fn hvcc(vps: &[u8], sps: &[u8], pps: &[u8]) -> Bytes {
		let mut out = BytesMut::new();
		out.extend_from_slice(&[0u8; 21]);
		out.extend_from_slice(&[0xff, 3]);
		for (nal_type, nal) in [(32u8, vps), (33, sps), (34, pps)] {
			out.extend_from_slice(&[0x80 | nal_type, 0, 1]);
			out.extend_from_slice(&(nal.len() as u16).to_be_bytes());
			out.extend_from_slice(nal);
		}
		out.freeze()
	}

	fn write_length_prefixed(group: &mut moq_net::group::Producer, timestamp_us: u64, nals: &[&[u8]]) {
		let mut payload = BytesMut::new();
		for nal in nals {
			payload.extend_from_slice(&(nal.len() as u32).to_be_bytes());
			payload.extend_from_slice(nal);
		}
		let frame = crate::container::Frame {
			timestamp: moq_net::Timestamp::from_micros(timestamp_us).unwrap(),
			duration: None,
			payload: payload.freeze(),
			keyframe: false,
		};
		<crate::catalog::hang::Container as crate::container::Container>::write(
			&crate::catalog::hang::Container::Legacy(crate::container::Kind::Data),
			group,
			&[frame],
		)
		.unwrap();
	}

	#[tokio::test(start_paused = true)]
	async fn a_moved_rendition_uses_its_new_parameter_sets() {
		use crate::container::test_util::{Moving, marked_frame};
		let mut moving = Moving::new();
		let first = hvc1_catalog("main", hvcc(&[0x40, 1, 0x0c], &[0x42, 1, 1, 0x60], &[0x44, 1, 0xc0]));
		let second = hvc1_catalog("main", hvcc(&[0x40, 1, 0x0d], &[0x42, 1, 1, 0x60], &[0x44, 1, 0xc0]));
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
	async fn hvc1_export_injects_vps_sps_pps_on_keyframes() {
		let vps = &[0x40, 0x01, 0x0c][..];
		let sps = &[0x42, 0x01, 0x01, 0x60][..];
		let pps = &[0x44, 0x01, 0xc0][..];
		let idr = &[0x26, 0x01, 0x88, 0x84][..];
		let trail = &[0x02, 0x01, 0xe0, 0x12][..];

		let catalog = hvc1_catalog("video.hvc1", hvcc(vps, sps, pps));
		let broadcast = moq_net::broadcast::Info::new().produce();
		let track = broadcast
			.create_track("video.hvc1", hang::container::track_info(hang::catalog::PRIORITY.video))
			.unwrap();

		let mut g0 = track.create_group(moq_net::group::Info { sequence: 0 }).unwrap();
		write_length_prefixed(&mut g0, 0, &[idr]);
		g0.finish().unwrap();

		let mut g1 = track.create_group(moq_net::group::Info { sequence: 1 }).unwrap();
		write_length_prefixed(&mut g1, 33_000, &[trail]);
		g1.finish().unwrap();
		track.finish().unwrap();

		let consumer = broadcast.consume();
		// The whole track is written before the exporter runs, so it needs a budget
		// wide enough to read it: the default skips everything but the live edge.
		let mut export = Export::new(crate::source::announced(&consumer).await, Once(Some(catalog)))
			.with_max_delay(std::time::Duration::from_secs(30));

		let frame0 = export.next().await.unwrap().expect("first frame");
		let frame1 = export.next().await.unwrap().expect("second frame");
		assert!(export.next().await.unwrap().is_none(), "track ended");

		let prefix = crate::codec::annexb::build_prefix(
			[
				Bytes::copy_from_slice(vps),
				Bytes::copy_from_slice(sps),
				Bytes::copy_from_slice(pps),
			]
			.iter(),
		);

		assert!(frame0.starts_with(&prefix), "frame 0 must begin with VPS/SPS/PPS");
		assert_eq!(&frame0[prefix.len()..], &[0, 0, 0, 1, 0x26, 0x01, 0x88, 0x84]);
		assert!(
			frame1.starts_with(&prefix),
			"group-start frame must begin with VPS/SPS/PPS"
		);
		assert_eq!(&frame1[prefix.len()..], &[0, 0, 0, 1, 0x02, 0x01, 0xe0, 0x12]);
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

		let mut config = VideoConfig::new(H265 {
			in_band: true,
			profile_space: 0,
			profile_idc: 1,
			profile_compatibility_flags: [0x60, 0, 0, 0],
			tier_flag: false,
			level_idc: 93,
			constraint_flags: [0x90, 0, 0, 0, 0, 0],
		});
		config.coded_width = Some(320);
		config.coded_height = Some(240);
		config.container = hang::catalog::Container::Legacy;
		let sibling_config = config.clone();
		let mut live = Live::new(".hev1", |catalog, name| {
			catalog.modify().unwrap().video.renditions.insert(name, config);
		});
		let primary = live.track.name().to_string();
		let mut sibling = live.add_track(".sibling", |catalog, name| {
			catalog.modify().unwrap().video.renditions.insert(name, sibling_config);
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
			let mut catalog = hvc1_catalog(
				"video",
				Bytes::from_static(&[1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0xff, 0]),
			);
			let config = catalog.video.renditions.get_mut("video").unwrap();
			let hang::catalog::VideoCodec::H265(codec) = &mut config.codec else {
				unreachable!()
			};
			codec.in_band = in_band;
			let broadcast = moq_net::broadcast::Info::new().produce();
			let track = broadcast
				.create_track("video", hang::container::track_info(hang::catalog::PRIORITY.video))
				.unwrap();
			let mut group = track.create_group(moq_net::group::Info { sequence: 0 }).unwrap();
			let nals: &[&[u8]] = &[&[0x40, 1, 0x0c], &[0x42, 1, 1], &[0x44, 1, 0xc0], &[0x26, 1, 0x88]];
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
					Err(crate::Error::H265(super::super::Error::MissingParamSets { .. }))
				));
			}
		}
	}
}
