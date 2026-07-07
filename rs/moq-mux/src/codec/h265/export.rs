//! H.265 single-rendition Annex-B exporter.
//!
//! HEVC analogue of [`crate::codec::h264::Export`]. Accepts either a hev1
//! (Annex-B, parameter sets inline) or hvc1 (length-prefixed + out-of-band
//! hvcC) source and emits a raw Annex-B elementary stream. Timestamps are
//! dropped.

use std::task::Poll;
use std::time::Duration;

use bytes::Bytes;
use hang::Catalog;
use hang::catalog::{VideoCodecKind, VideoConfig};

use crate::catalog::Stream;
use crate::codec::annexb;
use crate::container::ExportSource;

/// Single-rendition H.265 Annex-B exporter.
pub struct Export<S: Stream> {
	source: crate::Source,
	catalog: Option<S>,
	latency: Duration,
	track: Option<H265Track>,
}

struct H265Track {
	name: String,
	/// Snapshot of the catalog config we built `source` from. Cached so that
	/// a catalog update which keeps the same rendition name but changes the
	/// codec config (e.g. a new hvcC) triggers a full rebuild instead of
	/// silently reusing a stale `convert`.
	config: VideoConfig,
	source: ExportSource,
	/// `Some` for an hvc1 source: VPS/SPS/PPS prefix prebuilt from the hvcC,
	/// and the hvcC length-prefix size. `None` for a hev1 source: Annex-B
	/// passes through without conversion.
	convert: Option<Hvc1Convert>,
}

struct Hvc1Convert {
	length_size: usize,
	keyframe_prefix: Bytes,
}

impl<S: Stream> Export<S> {
	/// Subscribe to `source` and emit an Annex-B H.265 byte stream.
	///
	/// `catalog` is expected to be narrowed to a single H.265 rendition. If
	/// multiple H.265 renditions appear in a snapshot, the first by BTreeMap
	/// order wins and a warning is logged.
	pub fn new(source: impl Into<crate::Source>, catalog: S) -> Self {
		Self {
			source: source.into(),
			catalog: Some(catalog),
			latency: Duration::ZERO,
			track: None,
		}
	}

	/// Set the maximum buffering latency for the per-track source.
	pub fn with_latency(mut self, latency: Duration) -> Self {
		self.latency = latency;
		self
	}

	pub async fn next(&mut self) -> crate::Result<Option<Bytes>> {
		kio::wait(|waiter| self.poll_next(waiter)).await
	}

	pub fn poll_next(&mut self, waiter: &kio::Waiter) -> Poll<crate::Result<Option<Bytes>>> {
		while let Some(catalog) = self.catalog.as_mut() {
			match catalog.poll_next(waiter)? {
				Poll::Ready(Some(snapshot)) => self.update_catalog(&snapshot.media())?,
				Poll::Ready(None) => {
					self.catalog = None;
					break;
				}
				Poll::Pending => break,
			}
		}

		loop {
			let Some(track) = self.track.as_mut() else {
				if self.catalog.is_none() {
					return Poll::Ready(Ok(None));
				}
				return Poll::Pending;
			};

			match track.source.poll_read(waiter) {
				Poll::Ready(Ok(Some(frame))) => {
					let bytes = match &track.convert {
						None => frame.payload,
						Some(convert) => {
							let prefix = frame.keyframe.then(|| convert.keyframe_prefix.as_ref());
							annexb::from_length_prefixed(&frame.payload, convert.length_size, prefix)?
						}
					};
					if bytes.is_empty() {
						continue;
					}
					return Poll::Ready(Ok(Some(bytes)));
				}
				Poll::Ready(Ok(None)) => {
					self.track = None;
					continue;
				}
				Poll::Ready(Err(e)) => return Poll::Ready(Err(e)),
				Poll::Pending => return Poll::Pending,
			}
		}
	}

	fn update_catalog(&mut self, catalog: &Catalog) -> crate::Result<()> {
		let picked = catalog
			.video
			.renditions
			.iter()
			.filter(|(_, c)| c.codec.kind() == VideoCodecKind::H265)
			.collect::<Vec<_>>();

		if picked.len() > 1 {
			tracing::warn!(
				count = picked.len(),
				"multiple H.265 renditions in catalog snapshot; using the first by name. \
				 Narrow with catalog::Select to pick one explicitly."
			);
		}

		let Some((name, config)) = picked.into_iter().next() else {
			self.track = None;
			return Ok(());
		};

		if self
			.track
			.as_ref()
			.is_some_and(|t| t.name == *name && t.config == *config)
		{
			return Ok(());
		}

		let source = ExportSource::for_video_raw(&self.source, name, config, self.latency)?;
		let convert = match config.description.as_ref().filter(|d| !d.is_empty()) {
			None => None,
			Some(hvcc) => {
				let params = super::Hvcc::parse(hvcc)?;
				if params.vps.is_empty() || params.sps.is_empty() || params.pps.is_empty() {
					return Err(super::Error::MissingParamSets {
						name: name.clone(),
						vps: params.vps.len(),
						sps: params.sps.len(),
						pps: params.pps.len(),
					}
					.into());
				}
				let prefix = annexb::build_prefix(params.vps.iter().chain(params.sps.iter()).chain(params.pps.iter()));
				Some(Hvc1Convert {
					length_size: params.length_size,
					keyframe_prefix: prefix,
				})
			}
		};

		self.track = Some(H265Track {
			name: name.clone(),
			config: config.clone(),
			source,
			convert,
		});

		Ok(())
	}
}

#[cfg(test)]
mod tests {
	use std::collections::BTreeMap;
	use std::task::Poll;

	use bytes::{Bytes, BytesMut};
	use hang::catalog::{H265, Video, VideoConfig};

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

		let mut renditions = BTreeMap::new();
		renditions.insert(name.to_string(), config);

		Catalog {
			video: Video {
				renditions,
				display: None,
				rotation: None,
				flip: None,
			},
			..Default::default()
		}
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
			&crate::catalog::hang::Container::Legacy,
			group,
			&[frame],
		)
		.unwrap();
	}

	#[tokio::test(start_paused = true)]
	async fn hvc1_export_injects_vps_sps_pps_on_keyframes() {
		let vps = &[0x40, 0x01, 0x0c][..];
		let sps = &[0x42, 0x01, 0x01, 0x60][..];
		let pps = &[0x44, 0x01, 0xc0][..];
		let idr = &[0x26, 0x01, 0x88, 0x84][..];
		let trail = &[0x02, 0x01, 0xe0, 0x12][..];

		let catalog = hvc1_catalog("video.hvc1", hvcc(vps, sps, pps));
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let mut track = broadcast
			.create_track(
				"video.hvc1",
				moq_net::track::Info::default().with_timescale(hang::container::TIMESCALE),
			)
			.unwrap();

		let mut g0 = track.create_group(moq_net::group::Info { sequence: 0 }).unwrap();
		write_length_prefixed(&mut g0, 0, &[idr]);
		g0.finish().unwrap();

		let mut g1 = track.create_group(moq_net::group::Info { sequence: 1 }).unwrap();
		write_length_prefixed(&mut g1, 33_000, &[trail]);
		g1.finish().unwrap();
		track.finish().unwrap();

		let consumer = broadcast.consume();
		let mut export = Export::new(consumer, Once(Some(catalog)));

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
}
