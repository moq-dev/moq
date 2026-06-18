//! Single-codec importers.
//!
//! [`Track`] publishes one MoQ track from whole frames; [`TrackStream`] does the
//! same from a raw byte stream where frame boundaries have to be inferred. Both
//! own exactly one track, so they expose [`Track::demand`] / [`Track::name`]
//! directly rather than fallibly.

use crate::Result;

/// Build an H.264 avc3 split + import pair, resolving the config from `init`.
///
/// The import reads `init` for the codec config; the split then reads it as the
/// leading bytes of the stream (caching any inline SPS/PPS). Any frames in the
/// init buffer are published.
fn build_h264_avc3(
	track: moq_net::TrackProducer,
	catalog: crate::catalog::Producer,
	init: &[u8],
) -> Result<(crate::codec::h264::Split, crate::codec::h264::Import)> {
	let mut import = crate::codec::h264::Import::new(track, catalog);
	import.initialize(init)?;
	let mut split = crate::codec::h264::Split::new();
	let frames = split.decode(init, None)?;
	import.decode(frames)?;
	Ok((split, import))
}

/// Build an H.264 avc1 import, resolving the config and the NALU length size from
/// the avcC. avc1 has no splitter: each access unit is wrapped directly via
/// [`crate::codec::h264::avc1_frame`].
fn build_h264_avc1(
	track: moq_net::TrackProducer,
	catalog: crate::catalog::Producer,
	init: &[u8],
) -> Result<(usize, crate::codec::h264::Import)> {
	let mut import = crate::codec::h264::Import::new(track, catalog);
	import.initialize(init)?;
	let length_size = crate::codec::h264::Avcc::parse(init)?.length_size;
	Ok((length_size, import))
}

/// Build an H.265 split + import pair, resolving the config from `init`.
fn build_h265(
	track: moq_net::TrackProducer,
	catalog: crate::catalog::Producer,
	init: &[u8],
) -> Result<(crate::codec::h265::Split, crate::codec::h265::Import)> {
	let mut import = crate::codec::h265::Import::new(track, catalog);
	import.initialize(init)?;
	let mut split = crate::codec::h265::Split::new();
	let frames = split.decode(init, None)?;
	import.decode(frames)?;
	Ok((split, import))
}

/// Build an AV1 split + import pair, resolving the config from `init`.
fn build_av1(
	track: moq_net::TrackProducer,
	catalog: crate::catalog::Producer,
	init: &[u8],
) -> Result<(crate::codec::av1::Split, crate::codec::av1::Import)> {
	let mut import = crate::codec::av1::Import::new(track, catalog);
	import.initialize(init)?;
	let mut split = crate::codec::av1::Split::new();
	// av1C (leading 0x81, ISO/IEC 14496-15) is an out-of-band config record, not an
	// OBU stream, so it's read for config (above) and dropped here. Raw OBUs are the
	// leading bytes of the stream and feed the splitter.
	let frames = if init.len() >= 16 && init[0] == 0x81 {
		Vec::new()
	} else {
		split.decode(init, None)?
	};
	import.decode(frames)?;
	Ok((split, import))
}

enum TrackKind {
	/// H.264 avc3 (Annex-B, inline SPS/PPS). The split owns byte parsing; the
	/// import publishes.
	Avc3 {
		split: crate::codec::h264::Split,
		import: crate::codec::h264::Import,
	},
	/// H.264 avc1 (length-prefixed NALU, out-of-band avcC). No splitter: each
	/// access unit is wrapped directly. `length_size` is the NALU length prefix
	/// width read from the avcC.
	Avc1 {
		length_size: usize,
		import: crate::codec::h264::Import,
	},
	Hev1 {
		split: crate::codec::h265::Split,
		import: crate::codec::h265::Import,
	},
	Av01 {
		split: crate::codec::av1::Split,
		import: crate::codec::av1::Import,
	},
	Vp8(crate::codec::vp8::Import),
	Vp9(crate::codec::vp9::Import),
	Aac(crate::codec::aac::Import),
	Opus(crate::codec::opus::Import),
}

/// A single-codec importer for whole frames.
///
/// Use this when the caller already has whole frames (the typical case for files
/// and reassembled network input). Each [`decode`](Self::decode) call takes one
/// complete frame.
pub struct Track {
	kind: TrackKind,
}

impl Track {
	/// Create an importer that publishes a single codec onto a reserved track.
	///
	/// The caller reserves the track (by name) with
	/// [`BroadcastProducer::reserve_track`](moq_net::BroadcastProducer::reserve_track);
	/// the importer accepts it here, which is where the track's timescale is set.
	/// The catalog rendition is registered once the codec config is resolved.
	pub fn new(
		request: moq_net::TrackRequest,
		catalog: crate::catalog::Producer,
		format: &str,
		init: &[u8],
	) -> Result<Self> {
		// Accept at the legacy microsecond timescale, matching the frame timestamps
		// the container stamps. A codec-specific timescale (e.g. the opus sample
		// rate) would be chosen here instead.
		let track = request.accept(moq_net::TrackInfo::default().with_timescale(hang::container::TIMESCALE));
		let kind = match format {
			"avc1" | "avcc" => {
				let (length_size, import) = build_h264_avc1(track, catalog, init)?;
				TrackKind::Avc1 { length_size, import }
			}
			"avc3" | "h264" => {
				let (split, import) = build_h264_avc3(track, catalog, init)?;
				TrackKind::Avc3 { split, import }
			}
			"hev1" => {
				let (split, import) = build_h265(track, catalog, init)?;
				TrackKind::Hev1 { split, import }
			}
			"av01" | "av1" | "av1c" | "av1C" => {
				let (split, import) = build_av1(track, catalog, init)?;
				TrackKind::Av01 { split, import }
			}
			"vp8" | "vp08" => {
				let mut import = crate::codec::vp8::Import::new(track, catalog);
				import.initialize(init)?;
				TrackKind::Vp8(import)
			}
			"vp9" | "vp09" => {
				let mut import = crate::codec::vp9::Import::new(track, catalog);
				import.initialize(init)?;
				TrackKind::Vp9(import)
			}
			"aac" => {
				let mut data = init;
				let config = crate::codec::aac::Config::parse(&mut data)?;
				let import = crate::codec::aac::Import::new(track, catalog, config)?;
				TrackKind::Aac(import)
			}
			"opus" => {
				let mut data = init;
				let config = crate::codec::opus::Config::parse(&mut data)?;
				let import = crate::codec::opus::Import::new(track, catalog, config)?;
				TrackKind::Opus(import)
			}
			_ => return Err(crate::Error::UnknownFormat(format.to_string())),
		};

		Ok(Self { kind })
	}

	/// Decode one whole frame.
	pub fn decode(&mut self, frame: &[u8], pts: Option<moq_net::Timestamp>) -> Result<()> {
		match self.kind {
			TrackKind::Avc3 {
				ref mut split,
				ref mut import,
			} => {
				// One whole access unit per call, so flush to emit it rather than
				// waiting for the next start code.
				let mut frames = split.decode(frame, pts)?;
				frames.extend(split.flush(pts)?);
				import.decode(frames)?;
			}
			TrackKind::Avc1 {
				length_size,
				ref mut import,
			} => {
				let pts = pts.ok_or(crate::codec::h264::Error::MissingTimestamp)?;
				let frame = crate::codec::h264::avc1_frame(frame, length_size, pts)?;
				import.decode([frame])?;
			}
			TrackKind::Hev1 {
				ref mut split,
				ref mut import,
			} => {
				let mut frames = split.decode(frame, pts)?;
				frames.extend(split.flush(pts)?);
				import.decode(frames)?;
			}
			TrackKind::Av01 {
				ref mut split,
				ref mut import,
			} => {
				let mut frames = split.decode(frame, pts)?;
				frames.extend(split.flush(pts)?);
				import.decode(frames)?;
			}
			TrackKind::Vp8(ref mut import) => import.decode(frame, pts)?,
			TrackKind::Vp9(ref mut import) => import.decode(frame, pts)?,
			TrackKind::Aac(ref mut import) => import.decode(frame, pts)?,
			TrackKind::Opus(ref mut import) => import.decode(frame, pts)?,
		}

		Ok(())
	}

	/// Finish the importer, flushing any buffered data.
	pub fn finish(&mut self) -> Result<()> {
		match self.kind {
			TrackKind::Avc3 { ref mut import, .. } => import.finish(),
			TrackKind::Avc1 { ref mut import, .. } => import.finish(),
			TrackKind::Hev1 { ref mut import, .. } => import.finish(),
			TrackKind::Av01 { ref mut import, .. } => import.finish(),
			TrackKind::Vp8(ref mut import) => import.finish(),
			TrackKind::Vp9(ref mut import) => import.finish(),
			TrackKind::Aac(ref mut import) => import.finish(),
			TrackKind::Opus(ref mut import) => import.finish(),
		}
	}

	/// Close the current group and open the next one at `sequence`.
	pub fn seek(&mut self, sequence: u64) -> Result<()> {
		match self.kind {
			TrackKind::Avc3 {
				ref mut split,
				ref mut import,
			} => {
				split.reset();
				import.seek(sequence)
			}
			TrackKind::Avc1 { ref mut import, .. } => import.seek(sequence),
			TrackKind::Hev1 {
				ref mut split,
				ref mut import,
			} => {
				split.reset();
				import.seek(sequence)
			}
			TrackKind::Av01 {
				ref mut split,
				ref mut import,
			} => {
				split.reset();
				import.seek(sequence)
			}
			TrackKind::Vp8(ref mut import) => import.seek(sequence),
			TrackKind::Vp9(ref mut import) => import.seek(sequence),
			TrackKind::Aac(ref mut import) => import.seek(sequence),
			TrackKind::Opus(ref mut import) => import.seek(sequence),
		}
	}

	/// A watch-only handle to the track's subscriber demand.
	pub fn demand(&self) -> moq_net::TrackDemand {
		match self.kind {
			TrackKind::Avc3 { ref import, .. } => import.demand(),
			TrackKind::Avc1 { ref import, .. } => import.demand(),
			TrackKind::Hev1 { ref import, .. } => import.demand(),
			TrackKind::Av01 { ref import, .. } => import.demand(),
			TrackKind::Vp8(ref import) => import.demand(),
			TrackKind::Vp9(ref import) => import.demand(),
			TrackKind::Aac(ref import) => import.demand(),
			TrackKind::Opus(ref import) => import.demand(),
		}
	}

	/// The name of the track this importer publishes.
	pub fn name(&self) -> String {
		self.demand().name().to_string()
	}
}

// Lift an already-built opus importer into a `Track` so callers that build their
// config out-of-band (e.g. moq-gst, which constructs `opus::Config` from gstreamer
// caps instead of an OpusHead buffer) can keep using `.into()`.
impl From<crate::codec::opus::Import> for Track {
	fn from(opus: crate::codec::opus::Import) -> Self {
		Self {
			kind: TrackKind::Opus(opus),
		}
	}
}

impl From<crate::codec::aac::Import> for Track {
	fn from(aac: crate::codec::aac::Import) -> Self {
		Self {
			kind: TrackKind::Aac(aac),
		}
	}
}

enum TrackStreamKind {
	/// H.264 in avc3 wire shape (Annex-B with inline SPS/PPS). The split owns
	/// byte parsing; the import publishes.
	Avc3 {
		split: crate::codec::h264::Split,
		import: crate::codec::h264::Import,
	},
	Hev1 {
		split: crate::codec::h265::Split,
		import: crate::codec::h265::Import,
	},
	Av01 {
		split: crate::codec::av1::Split,
		import: crate::codec::av1::Import,
	},
}

/// A single-codec importer for a raw byte stream with unknown frame boundaries.
///
/// Use this when the caller does not know the frame boundaries (piped Annex-B
/// H.264, an fMP4 reader, …); the importer infers them.
pub struct TrackStream {
	kind: TrackStreamKind,
}

impl TrackStream {
	/// Create an importer that publishes a single codec onto a reserved track.
	///
	/// The caller reserves the track with
	/// [`BroadcastProducer::reserve_track`](moq_net::BroadcastProducer::reserve_track);
	/// the importer accepts it here at the legacy microsecond timescale (where a
	/// codec-specific timescale would be chosen).
	pub fn new(request: moq_net::TrackRequest, catalog: crate::catalog::Producer, format: &str) -> Result<Self> {
		let track = request.accept(moq_net::TrackInfo::default().with_timescale(hang::container::TIMESCALE));
		// Only the self-delimiting codecs can be recovered from a raw byte stream.
		let kind = match format {
			"avc3" | "h264" => TrackStreamKind::Avc3 {
				split: crate::codec::h264::Split::new(),
				import: crate::codec::h264::Import::new(track, catalog),
			},
			"hev1" => TrackStreamKind::Hev1 {
				split: crate::codec::h265::Split::new(),
				import: crate::codec::h265::Import::new(track, catalog),
			},
			"av01" | "av1" | "av1c" | "av1C" => TrackStreamKind::Av01 {
				split: crate::codec::av1::Split::new(),
				import: crate::codec::av1::Import::new(track, catalog),
			},
			_ => return Err(crate::Error::UnknownFormat(format.to_string())),
		};

		Ok(Self { kind })
	}

	/// Initialize the importer with the given buffer and populate the broadcast.
	///
	/// This is not required for self-describing formats like AVC3.
	pub fn initialize(&mut self, data: &[u8]) -> Result<()> {
		match self.kind {
			TrackStreamKind::Avc3 {
				ref mut split,
				ref mut import,
			} => {
				import.initialize(data)?;
				let frames = split.decode(data, None)?;
				import.decode(frames)?;
			}
			TrackStreamKind::Hev1 {
				ref mut split,
				ref mut import,
			} => {
				import.initialize(data)?;
				let frames = split.decode(data, None)?;
				import.decode(frames)?;
			}
			TrackStreamKind::Av01 {
				ref mut split,
				ref mut import,
			} => {
				import.initialize(data)?;
				// av1C (leading 0x81) is an out-of-band config record, not an OBU
				// stream; read for config above and dropped here.
				let frames = if data.len() >= 16 && data[0] == 0x81 {
					Vec::new()
				} else {
					split.decode(data, None)?
				};
				import.decode(frames)?;
			}
		}

		Ok(())
	}

	/// Decode a chunk of the byte stream.
	pub fn decode(&mut self, data: &[u8]) -> Result<()> {
		match self.kind {
			TrackStreamKind::Avc3 {
				ref mut split,
				ref mut import,
			} => {
				let frames = split.decode(data, None)?;
				import.decode(frames)
			}
			TrackStreamKind::Hev1 {
				ref mut split,
				ref mut import,
			} => {
				let frames = split.decode(data, None)?;
				import.decode(frames)
			}
			TrackStreamKind::Av01 {
				ref mut split,
				ref mut import,
			} => {
				let frames = split.decode(data, None)?;
				import.decode(frames)
			}
		}
	}

	/// Finish the importer, flushing any buffered data.
	pub fn finish(&mut self) -> Result<()> {
		match self.kind {
			TrackStreamKind::Avc3 {
				ref mut split,
				ref mut import,
			} => {
				let tail = split.flush(None)?;
				import.decode(tail)?;
				import.finish()
			}
			TrackStreamKind::Hev1 {
				ref mut split,
				ref mut import,
			} => {
				let tail = split.flush(None)?;
				import.decode(tail)?;
				import.finish()
			}
			TrackStreamKind::Av01 {
				ref mut split,
				ref mut import,
			} => {
				let tail = split.flush(None)?;
				import.decode(tail)?;
				import.finish()
			}
		}
	}

	/// Close the current group and open the next one at `sequence`.
	pub fn seek(&mut self, sequence: u64) -> Result<()> {
		match self.kind {
			TrackStreamKind::Avc3 {
				ref mut split,
				ref mut import,
			} => {
				split.reset();
				import.seek(sequence)
			}
			TrackStreamKind::Hev1 {
				ref mut split,
				ref mut import,
			} => {
				split.reset();
				import.seek(sequence)
			}
			TrackStreamKind::Av01 {
				ref mut split,
				ref mut import,
			} => {
				split.reset();
				import.seek(sequence)
			}
		}
	}

	/// A watch-only handle to the track's subscriber demand.
	pub fn demand(&self) -> moq_net::TrackDemand {
		match self.kind {
			TrackStreamKind::Avc3 { ref import, .. } => import.demand(),
			TrackStreamKind::Hev1 { ref import, .. } => import.demand(),
			TrackStreamKind::Av01 { ref import, .. } => import.demand(),
		}
	}

	/// The name of the track this importer publishes.
	pub fn name(&self) -> String {
		self.demand().name().to_string()
	}
}

#[cfg(test)]
mod tests {
	use std::time::Duration;

	use super::*;
	use moq_net::Timestamp;

	fn opus_head() -> Vec<u8> {
		let mut head = Vec::with_capacity(19);
		head.extend_from_slice(b"OpusHead");
		head.push(1);
		head.push(2);
		head.extend_from_slice(&0u16.to_le_bytes());
		head.extend_from_slice(&48000u32.to_le_bytes());
		head.extend_from_slice(&0u16.to_le_bytes());
		head.push(0);
		head
	}

	fn h264_init() -> Vec<u8> {
		let mut init = Vec::new();
		init.extend_from_slice(&[0x00, 0x00, 0x00, 0x01]);
		init.extend_from_slice(&[
			0x67, 0x64, 0x00, 0x1f, 0xac, 0x24, 0x84, 0x01, 0x40, 0x16, 0xec, 0x04, 0x40, 0x00, 0x00, 0x03, 0x00, 0x40,
			0x00, 0x00, 0x0c, 0x23, 0xc6, 0x0c, 0x92,
		]);
		init.extend_from_slice(&[0x00, 0x00, 0x00, 0x01]);
		init.extend_from_slice(&[0x68, 0xee, 0x32, 0xc8, 0xb0]);
		init
	}

	fn new_broadcast() -> (moq_net::BroadcastProducer, crate::catalog::Producer) {
		let mut broadcast = moq_net::BroadcastInfo::new().produce();
		let catalog = crate::catalog::Producer::new(&mut broadcast).unwrap();
		(broadcast, catalog)
	}

	#[tokio::test(start_paused = true)]
	async fn existing_track_opus_uses_existing_name() {
		let (mut broadcast, catalog) = new_broadcast();
		// The importer accepts the reserved track, setting its (microsecond) timescale.
		let request = broadcast.reserve_track("requested-audio").unwrap();
		let mut import = Track::new(request, catalog.clone(), "opus", &opus_head()).unwrap();

		assert_eq!(import.name(), "requested-audio");
		let snapshot = catalog.snapshot();
		assert!(snapshot.audio.renditions.contains_key("requested-audio"));
		assert!(!snapshot.audio.renditions.contains_key("0.opus"));

		// Frame delivery and the accepted timescale are covered by `opus_import_delivers_frames`.
		import
			.decode(b"opus payload", Some(Timestamp::from_micros(1_000).unwrap()))
			.unwrap();
		import.finish().unwrap();
	}

	#[tokio::test(start_paused = true)]
	async fn unique_track_opus_attaches_catalog_and_retires_on_drop() {
		let (mut broadcast, catalog) = new_broadcast();

		// A freshly reserved track attaches its catalog rendition on init.
		let name = broadcast.unique_name(".opus");
		let request = broadcast.reserve_track(name).unwrap();
		let mut import = Track::new(request, catalog.clone(), "opus", &opus_head()).unwrap();

		assert_eq!(import.name(), "0.opus");
		assert!(catalog.snapshot().audio.renditions.contains_key("0.opus"));

		import
			.decode(b"opus payload", Some(Timestamp::from_micros(2_000).unwrap()))
			.unwrap();
		import.finish().unwrap();

		// Dropping the importer retires its rendition from the shared catalog.
		drop(import);
		assert!(!catalog.snapshot().audio.renditions.contains_key("0.opus"));
	}

	#[tokio::test(start_paused = true)]
	async fn opus_import_delivers_frames() {
		let (mut broadcast, catalog) = new_broadcast();
		let track = broadcast
			.create_track(
				"audio",
				moq_net::TrackInfo::default().with_timescale(hang::container::TIMESCALE),
			)
			.unwrap();
		let subscriber = track.subscribe(None);

		let config = crate::codec::opus::Config {
			sample_rate: 48_000,
			channel_count: 2,
		};
		let mut import = crate::codec::opus::Import::new(track, catalog.clone(), config).unwrap();
		assert!(catalog.snapshot().audio.renditions.contains_key("audio"));

		let mut media = crate::container::Consumer::new(subscriber, crate::catalog::hang::Container::Legacy);

		let payload = b"opus payload".to_vec();
		import
			.decode(&payload, Some(Timestamp::from_micros(1_000).unwrap()))
			.unwrap();

		let frame = tokio::time::timeout(Duration::from_secs(1), media.read())
			.await
			.unwrap()
			.unwrap()
			.unwrap();
		assert_eq!(frame.payload, payload);
		assert_eq!(frame.timestamp, Timestamp::from_micros(1_000).unwrap());

		import.finish().unwrap();
	}

	#[tokio::test(start_paused = true)]
	async fn existing_track_h264_uses_existing_name_in_catalog() {
		let (mut broadcast, catalog) = new_broadcast();
		let request = broadcast.reserve_track("camera").unwrap();

		let import = Track::new(request, catalog.clone(), "avc3", &h264_init()).unwrap();

		assert_eq!(import.name(), "camera");
		let snapshot = catalog.snapshot();
		let video = snapshot.video.renditions.get("camera").unwrap();
		assert_eq!(video.coded_width, Some(1280));
		assert_eq!(video.coded_height, Some(720));
		assert!(!snapshot.video.renditions.contains_key("0.avc3"));
	}

	/// A changed key frame just updates the rendition in place; there are no fixed
	/// tracks to reject a reconfiguration, so the second key frame succeeds.
	#[tokio::test(start_paused = true)]
	async fn reconfiguration_updates_in_place() {
		let (mut broadcast, catalog) = new_broadcast();
		let request = broadcast.reserve_track("video").unwrap();
		let mut import = Track::new(request, catalog, "vp8", &[]).unwrap();

		import
			.decode(
				&[0x10, 0x00, 0x00, 0x9d, 0x01, 0x2a, 0x40, 0x01, 0xf0, 0x00],
				Some(Timestamp::from_micros(0).unwrap()),
			)
			.unwrap();

		import
			.decode(
				&[0x10, 0x00, 0x00, 0x9d, 0x01, 0x2a, 0x80, 0x02, 0xe0, 0x01],
				Some(Timestamp::from_micros(33_000).unwrap()),
			)
			.unwrap();
	}
}
