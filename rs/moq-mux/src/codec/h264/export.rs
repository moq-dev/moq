//! H.264 single-rendition Annex-B exporter.
//!
//! Subscribes to one H.264 rendition from a catalog-narrowed stream and emits
//! a raw Annex-B elementary stream. Suitable for piping into `ffmpeg`, decoder
//! fuzzers, or recording one codec to disk. There is no container framing
//! (timestamps are dropped).
//!
//! Two source shapes are accepted:
//! - **avc3** (catalog `description` empty): payload is already Annex-B with
//!   SPS/PPS inline. Pass through unchanged.
//! - **avc1** (catalog `description` is the avcC): length-prefixed NALUs.
//!   Length prefixes are replaced with `00 00 00 01` start codes; SPS/PPS
//!   extracted from the avcC are injected ahead of every keyframe.

use std::task::Poll;
use std::time::Duration;

use bytes::Bytes;
use hang::Catalog;
use hang::catalog::{VideoCodecKind, VideoConfig};

use crate::catalog::Stream;
use crate::codec::annexb;
use crate::container::ExportSource;

/// Single-rendition H.264 Annex-B exporter.
pub struct Export<S: Stream> {
	broadcast: moq_net::BroadcastConsumer,
	catalog: Option<S>,
	latency: Duration,
	track: Option<H264Track>,
}

struct H264Track {
	name: String,
	/// Snapshot of the catalog config we built `source` from. Cached so that
	/// a catalog update which keeps the same rendition name but changes the
	/// codec config (e.g. a new avcC) triggers a full rebuild instead of
	/// silently reusing a stale `convert`.
	config: VideoConfig,
	source: ExportSource,
	/// `Some` for an avc1 source: SPS/PPS prefix prebuilt from the avcC, and
	/// the avcC length-prefix size. `None` for an avc3 source: Annex-B passes
	/// through without conversion.
	convert: Option<Avc1Convert>,
}

struct Avc1Convert {
	length_size: usize,
	keyframe_prefix: Bytes,
}

impl<S: Stream> Export<S> {
	/// Subscribe to `broadcast` and emit an Annex-B H.264 byte stream.
	///
	/// `catalog` is expected to be narrowed to a single H.264 rendition (e.g.
	/// `consumer.filter()` with `codec = H264` then `.target()` for ABR
	/// selection). Renditions of other codecs are ignored; if multiple H.264
	/// renditions appear in a snapshot, the first by BTreeMap order wins and
	/// a warning is logged.
	pub fn new(broadcast: moq_net::BroadcastConsumer, catalog: S) -> Self {
		Self {
			broadcast,
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
				Poll::Ready(Some(snapshot)) => self.update_catalog(&snapshot)?,
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
			.filter(|(_, c)| c.codec.kind() == VideoCodecKind::H264)
			.collect::<Vec<_>>();

		if picked.len() > 1 {
			tracing::warn!(
				count = picked.len(),
				"multiple H.264 renditions in catalog snapshot; using the first by name. \
				 Narrow with catalog::Target to pick one explicitly."
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

		let source = ExportSource::for_video_raw(&self.broadcast, name, config, self.latency)?;
		let convert = match config.description.as_ref().filter(|d| !d.is_empty()) {
			None => None,
			Some(avcc) => {
				let params = super::parse_avcc_param_sets(avcc)?;
				if params.sps.is_empty() || params.pps.is_empty() {
					return Err(super::Error::MissingParamSets {
						name: name.clone(),
						sps: params.sps.len(),
						pps: params.pps.len(),
					}
					.into());
				}
				let prefix = annexb::build_prefix(params.sps.iter().chain(params.pps.iter()));
				Some(Avc1Convert {
					length_size: params.length_size,
					keyframe_prefix: prefix,
				})
			}
		};

		self.track = Some(H264Track {
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

	use bytes::Bytes;
	use hang::Catalog;
	use hang::catalog::{H264, Video, VideoConfig};

	use super::*;
	use crate::catalog::Stream;

	/// One-shot Stream that yields a single catalog snapshot then closes.
	struct Once(Option<Catalog>);

	impl Stream for Once {
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

	/// Build a minimal avcC carrying one SPS + one PPS.
	fn build_avcc(sps: &[u8], pps: &[u8]) -> Bytes {
		super::super::build_avcc(&Bytes::copy_from_slice(sps), &Bytes::copy_from_slice(pps)).unwrap()
	}

	/// Write a length-prefixed (4-byte) NAL frame onto a moq-net group via
	/// the Legacy wire codec.
	fn write_length_prefixed(group: &mut moq_net::GroupProducer, timestamp_us: u64, nals: &[&[u8]]) {
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
			&crate::catalog::hang::Container::Legacy,
			group,
			&[frame],
		)
		.unwrap();
	}

	/// Regression: when source is avc1 (length-prefixed + out-of-band avcC),
	/// the exporter must inject SPS+PPS before every keyframe and convert
	/// length prefixes to start codes for every frame.
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
		let mut broadcast = moq_net::Broadcast::new().produce();
		let mut track = broadcast
			.create_track(moq_net::Track::new("video.m4s").with_timescale(hang::container::TIMESCALE))
			.unwrap();

		// Group 0 (keyframe-starting group): one IDR frame.
		let mut g0 = track.create_group(moq_net::Group { sequence: 0 }).unwrap();
		write_length_prefixed(&mut g0, 0, &[idr]);
		g0.finish().unwrap();

		// Group 1 (next group): one P-slice. Consumer marks the first frame
		// of every group as keyframe by protocol invariant, so the exporter
		// MUST treat both group-starts as keyframes and inject SPS+PPS twice.
		let mut g1 = track.create_group(moq_net::Group { sequence: 1 }).unwrap();
		write_length_prefixed(&mut g1, 33_000, &[p_slice]);
		g1.finish().unwrap();
		track.finish().unwrap();

		// Consumer side: run the exporter.
		let consumer = broadcast.consume();
		let mut export = Export::new(consumer, Once(Some(catalog)));

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
}
