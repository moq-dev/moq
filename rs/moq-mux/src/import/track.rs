//! Single-codec importers.
//!
//! [`Track`] publishes one MoQ track from whole frames; [`TrackStream`] does the
//! same from a raw byte stream where frame boundaries have to be inferred. Both
//! own exactly one track, so they expose [`Track::demand`] / [`Track::name`]
//! directly rather than fallibly.

use std::marker::PhantomData;

use crate::Result;
use crate::catalog::hang::CatalogExt;
use crate::codec::{av1, h264, h265};
use crate::container::{Frame, Timestamp};

/// Object-safe dispatch for a [`Track`] importer (whole frames).
trait Importer: Send {
	fn decode(&mut self, frame: &[u8], pts: Option<Timestamp>) -> Result<()>;
	fn finish(&mut self) -> Result<()>;
	fn seek(&mut self, sequence: u64) -> Result<()>;
	fn demand(&self) -> moq_net::TrackDemand;
}

/// Object-safe dispatch for a [`TrackStream`] importer (raw byte stream).
trait StreamImporter: Send {
	fn initialize(&mut self, data: &[u8]) -> Result<()>;
	fn decode(&mut self, data: &[u8]) -> Result<()>;
	fn finish(&mut self) -> Result<()>;
	fn seek(&mut self, sequence: u64) -> Result<()>;
	fn demand(&self) -> moq_net::TrackDemand;
}

/// The Annex-B / OBU splitter shared by H.264, H.265, and AV1.
trait Splitter: Send {
	fn decode(&mut self, data: &[u8], pts: Option<Timestamp>) -> Result<Vec<Frame>>;
	fn flush(&mut self, pts: Option<Timestamp>) -> Result<Vec<Frame>>;
	fn reset(&mut self);
}

/// A codec importer fed pre-split access units (H.264, H.265, AV1).
trait FrameSink: Send {
	fn initialize(&mut self, init: &[u8]) -> Result<()>;
	fn decode(&mut self, frames: Vec<Frame>) -> Result<()>;
	fn finish(&mut self) -> Result<()>;
	fn seek(&mut self, sequence: u64) -> Result<()>;
	fn demand(&self) -> moq_net::TrackDemand;
}

macro_rules! impl_splitter {
	($ty:ty) => {
		impl Splitter for $ty {
			fn decode(&mut self, data: &[u8], pts: Option<Timestamp>) -> Result<Vec<Frame>> {
				<$ty>::decode(self, data, pts)
			}
			fn flush(&mut self, pts: Option<Timestamp>) -> Result<Vec<Frame>> {
				<$ty>::flush(self, pts)
			}
			fn reset(&mut self) {
				<$ty>::reset(self)
			}
		}
	};
}
impl_splitter!(h264::Split);
impl_splitter!(h265::Split);
impl_splitter!(av1::Split);

macro_rules! impl_frame_sink {
	($ty:ty) => {
		impl<E: CatalogExt> FrameSink for $ty {
			fn initialize(&mut self, init: &[u8]) -> Result<()> {
				<$ty>::initialize(self, init)
			}
			fn decode(&mut self, frames: Vec<Frame>) -> Result<()> {
				<$ty>::decode(self, frames)
			}
			fn finish(&mut self) -> Result<()> {
				<$ty>::finish(self)
			}
			fn seek(&mut self, sequence: u64) -> Result<()> {
				<$ty>::seek(self, sequence)
			}
			fn demand(&self) -> moq_net::TrackDemand {
				<$ty>::demand(self)
			}
		}
	};
}
impl_frame_sink!(h264::Import<E>);
impl_frame_sink!(h265::Import<E>);
impl_frame_sink!(av1::Import<E>);

/// Whole-frame split importer: each call is one access unit, so flush to emit it
/// rather than waiting for the next start code.
struct SplitWhole<S, I> {
	split: S,
	import: I,
}

impl<S: Splitter, I: FrameSink> Importer for SplitWhole<S, I> {
	fn decode(&mut self, frame: &[u8], pts: Option<Timestamp>) -> Result<()> {
		let mut frames = self.split.decode(frame, pts)?;
		frames.extend(self.split.flush(pts)?);
		self.import.decode(frames)
	}
	fn finish(&mut self) -> Result<()> {
		self.import.finish()
	}
	fn seek(&mut self, sequence: u64) -> Result<()> {
		self.split.reset();
		self.import.seek(sequence)
	}
	fn demand(&self) -> moq_net::TrackDemand {
		self.import.demand()
	}
}

/// Byte-stream split importer: infer frame boundaries from a raw stream, flushing
/// only on [`finish`](StreamImporter::finish).
struct SplitStream<S, I> {
	split: S,
	import: I,
	/// True for AV1: the leading bytes are an out-of-band av1C config record, read
	/// for config and dropped from the splitter rather than parsed as an OBU stream.
	skip_config_record: bool,
}

impl<S: Splitter, I: FrameSink> StreamImporter for SplitStream<S, I> {
	fn initialize(&mut self, data: &[u8]) -> Result<()> {
		self.import.initialize(data)?;
		let frames = if self.skip_config_record && is_av1c(data) {
			Vec::new()
		} else {
			self.split.decode(data, None)?
		};
		self.import.decode(frames)
	}
	fn decode(&mut self, data: &[u8]) -> Result<()> {
		let frames = self.split.decode(data, None)?;
		self.import.decode(frames)
	}
	fn finish(&mut self) -> Result<()> {
		let tail = self.split.flush(None)?;
		self.import.decode(tail)?;
		self.import.finish()
	}
	fn seek(&mut self, sequence: u64) -> Result<()> {
		self.split.reset();
		self.import.seek(sequence)
	}
	fn demand(&self) -> moq_net::TrackDemand {
		self.import.demand()
	}
}

/// avc1 (length-prefixed NALU, out-of-band avcC). No splitter: each access unit is
/// wrapped directly via [`avc1_frame`](h264::avc1_frame).
struct Avc1<E: CatalogExt> {
	length_size: usize,
	import: h264::Import<E>,
}

impl<E: CatalogExt> Importer for Avc1<E> {
	fn decode(&mut self, frame: &[u8], pts: Option<Timestamp>) -> Result<()> {
		let pts = pts.ok_or(h264::Error::MissingTimestamp)?;
		let frame = h264::avc1_frame(frame, self.length_size, pts)?;
		self.import.decode([frame])
	}
	fn finish(&mut self) -> Result<()> {
		self.import.finish()
	}
	fn seek(&mut self, sequence: u64) -> Result<()> {
		self.import.seek(sequence)
	}
	fn demand(&self) -> moq_net::TrackDemand {
		self.import.demand()
	}
}

/// The codecs that carry their config in-band and need no splitter: each call wraps
/// one whole frame directly.
macro_rules! impl_importer_direct {
	($ty:ty) => {
		impl<E: CatalogExt> Importer for $ty {
			fn decode(&mut self, frame: &[u8], pts: Option<Timestamp>) -> Result<()> {
				<$ty>::decode(self, frame, pts)
			}
			fn finish(&mut self) -> Result<()> {
				<$ty>::finish(self)
			}
			fn seek(&mut self, sequence: u64) -> Result<()> {
				<$ty>::seek(self, sequence)
			}
			fn demand(&self) -> moq_net::TrackDemand {
				<$ty>::demand(self)
			}
		}
	};
}
impl_importer_direct!(crate::codec::vp8::Import<E>);
impl_importer_direct!(crate::codec::vp9::Import<E>);
impl_importer_direct!(crate::codec::aac::Import<E>);
impl_importer_direct!(crate::codec::opus::Import<E>);
impl_importer_direct!(crate::codec::mp3::Import<E>);

/// An av1C config record (ISO/IEC 14496-15) starts with a 0x81 marker and is at
/// least 16 bytes; raw OBUs never look like this.
fn is_av1c(data: &[u8]) -> bool {
	data.len() >= 16 && data[0] == 0x81
}

/// Build an H.264 avc3 split + import pair, resolving the config from `init`.
///
/// The import reads `init` for the codec config; the split then reads it as the
/// leading bytes of the stream (caching any inline SPS/PPS). Any frames in the
/// init buffer are published.
fn build_h264_avc3<E: CatalogExt>(
	track: moq_net::TrackProducer,
	catalog: crate::catalog::Producer<E>,
	init: &[u8],
) -> Result<(crate::codec::h264::Split, crate::codec::h264::Import<E>)> {
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
fn build_h264_avc1<E: CatalogExt>(
	track: moq_net::TrackProducer,
	catalog: crate::catalog::Producer<E>,
	init: &[u8],
) -> Result<(usize, crate::codec::h264::Import<E>)> {
	let mut import = crate::codec::h264::Import::new(track, catalog);
	import.initialize(init)?;
	let length_size = crate::codec::h264::Avcc::parse(init)?.length_size;
	Ok((length_size, import))
}

/// Build an H.265 split + import pair, resolving the config from `init`.
fn build_h265<E: CatalogExt>(
	track: moq_net::TrackProducer,
	catalog: crate::catalog::Producer<E>,
	init: &[u8],
) -> Result<(crate::codec::h265::Split, crate::codec::h265::Import<E>)> {
	let mut import = crate::codec::h265::Import::new(track, catalog);
	import.initialize(init)?;
	let mut split = crate::codec::h265::Split::new();
	let frames = split.decode(init, None)?;
	import.decode(frames)?;
	Ok((split, import))
}

/// Build an AV1 split + import pair, resolving the config from `init`.
fn build_av1<E: CatalogExt>(
	track: moq_net::TrackProducer,
	catalog: crate::catalog::Producer<E>,
	init: &[u8],
) -> Result<(crate::codec::av1::Split, crate::codec::av1::Import<E>)> {
	let mut import = crate::codec::av1::Import::new(track, catalog);
	import.initialize(init)?;
	let mut split = crate::codec::av1::Split::new();
	// av1C (ISO/IEC 14496-15) is an out-of-band config record, not an OBU stream, so it's
	// read for config (above) and dropped here. Raw OBUs are the leading bytes of the
	// stream and feed the splitter.
	let frames = if is_av1c(init) {
		Vec::new()
	} else {
		split.decode(init, None)?
	};
	import.decode(frames)?;
	Ok((split, import))
}

/// A single-codec importer for whole frames.
///
/// Use this when the caller already has whole frames (the typical case for files
/// and reassembled network input). Each [`decode`](Self::decode) call takes one
/// complete frame.
pub struct Track<E: CatalogExt = ()> {
	inner: Box<dyn Importer>,
	_ext: PhantomData<E>,
}

impl<E: CatalogExt> Track<E> {
	/// Create an importer that publishes a single codec onto an existing track.
	///
	/// The caller mints the track (by name) with
	/// [`BroadcastProducer::create_track`](moq_net::BroadcastProducer::create_track) (or
	/// [`unique_track`](crate::import::unique_track)) and hands it here.
	/// The catalog rendition is registered once the codec config is resolved.
	pub fn new(
		track: moq_net::TrackProducer,
		catalog: crate::catalog::Producer<E>,
		format: &str,
		init: &[u8],
	) -> Result<Self> {
		let inner: Box<dyn Importer> = match format {
			"avc1" | "avcc" => {
				let (length_size, import) = build_h264_avc1(track, catalog, init)?;
				Box::new(Avc1 { length_size, import })
			}
			"avc3" | "h264" => {
				let (split, import) = build_h264_avc3(track, catalog, init)?;
				Box::new(SplitWhole { split, import })
			}
			"hev1" => {
				let (split, import) = build_h265(track, catalog, init)?;
				Box::new(SplitWhole { split, import })
			}
			"av01" | "av1" | "av1c" | "av1C" => {
				let (split, import) = build_av1(track, catalog, init)?;
				Box::new(SplitWhole { split, import })
			}
			"vp8" | "vp08" => {
				let mut import = crate::codec::vp8::Import::new(track, catalog);
				import.initialize(init)?;
				Box::new(import)
			}
			"vp9" | "vp09" => {
				let mut import = crate::codec::vp9::Import::new(track, catalog);
				import.initialize(init)?;
				Box::new(import)
			}
			"aac" => {
				let mut data = init;
				let config = crate::codec::aac::Config::parse(&mut data)?;
				Box::new(crate::codec::aac::Import::new(track, catalog, config)?)
			}
			"opus" => {
				let mut data = init;
				let config = crate::codec::opus::Config::parse(&mut data)?;
				Box::new(crate::codec::opus::Import::new(track, catalog, config)?)
			}
			_ => return Err(crate::Error::UnknownFormat(format.to_string())),
		};

		Ok(Self {
			inner,
			_ext: PhantomData,
		})
	}

	/// Decode one whole frame.
	pub fn decode(&mut self, frame: &[u8], pts: Option<Timestamp>) -> Result<()> {
		self.inner.decode(frame, pts)
	}

	/// Finish the importer, flushing any buffered data.
	pub fn finish(&mut self) -> Result<()> {
		self.inner.finish()
	}

	/// Close the current group and open the next one at `sequence`.
	pub fn seek(&mut self, sequence: u64) -> Result<()> {
		self.inner.seek(sequence)
	}

	/// A watch-only handle to the track's subscriber demand.
	pub fn demand(&self) -> moq_net::TrackDemand {
		self.inner.demand()
	}

	/// The name of the track this importer publishes.
	pub fn name(&self) -> String {
		self.demand().name().to_string()
	}
}

// Lift an already-built opus importer into a `Track` so callers that build their
// config out-of-band (e.g. moq-gst, which constructs `opus::Config` from gstreamer
// caps instead of an OpusHead buffer) can keep using `.into()`.
impl<E: CatalogExt> From<crate::codec::opus::Import<E>> for Track<E> {
	fn from(opus: crate::codec::opus::Import<E>) -> Self {
		Self {
			inner: Box::new(opus),
			_ext: PhantomData,
		}
	}
}

impl<E: CatalogExt> From<crate::codec::aac::Import<E>> for Track<E> {
	fn from(aac: crate::codec::aac::Import<E>) -> Self {
		Self {
			inner: Box::new(aac),
			_ext: PhantomData,
		}
	}
}

// Lift an already-built mp3 importer into a `Track` so callers that build their
// config out-of-band (e.g. moq-gst, which reads rate/channels from gstreamer caps
// rather than parsing a frame header) can keep using `.into()`.
impl<E: CatalogExt> From<crate::codec::mp3::Import<E>> for Track<E> {
	fn from(mp3: crate::codec::mp3::Import<E>) -> Self {
		Self {
			inner: Box::new(mp3),
			_ext: PhantomData,
		}
	}
}

/// A single-codec importer for a raw byte stream with unknown frame boundaries.
///
/// Use this when the caller does not know the frame boundaries (piped Annex-B
/// H.264, an fMP4 reader, …); the importer infers them.
pub struct TrackStream<E: CatalogExt = ()> {
	inner: Box<dyn StreamImporter>,
	_ext: PhantomData<E>,
}

impl<E: CatalogExt> TrackStream<E> {
	/// Create an importer that publishes a single codec onto an existing track.
	///
	/// The caller mints the track with
	/// [`BroadcastProducer::create_track`](moq_net::BroadcastProducer::create_track) (or
	/// [`unique_track`](crate::import::unique_track)) and hands it here; frames are stamped at
	/// the legacy microsecond timescale.
	pub fn new(track: moq_net::TrackProducer, catalog: crate::catalog::Producer<E>, format: &str) -> Result<Self> {
		// Only the self-delimiting codecs can be recovered from a raw byte stream.
		let inner: Box<dyn StreamImporter> = match format {
			"avc3" | "h264" => Box::new(SplitStream {
				split: h264::Split::new(),
				import: h264::Import::new(track, catalog),
				skip_config_record: false,
			}),
			"hev1" => Box::new(SplitStream {
				split: h265::Split::new(),
				import: h265::Import::new(track, catalog),
				skip_config_record: false,
			}),
			"av01" | "av1" | "av1c" | "av1C" => Box::new(SplitStream {
				split: av1::Split::new(),
				import: av1::Import::new(track, catalog),
				skip_config_record: true,
			}),
			_ => return Err(crate::Error::UnknownFormat(format.to_string())),
		};

		Ok(Self {
			inner,
			_ext: PhantomData,
		})
	}

	/// Initialize the importer with the given buffer and populate the broadcast.
	///
	/// This is not required for self-describing formats like AVC3.
	pub fn initialize(&mut self, data: &[u8]) -> Result<()> {
		self.inner.initialize(data)
	}

	/// Decode a chunk of the byte stream.
	pub fn decode(&mut self, data: &[u8]) -> Result<()> {
		self.inner.decode(data)
	}

	/// Finish the importer, flushing any buffered data.
	pub fn finish(&mut self) -> Result<()> {
		self.inner.finish()
	}

	/// Close the current group and open the next one at `sequence`.
	pub fn seek(&mut self, sequence: u64) -> Result<()> {
		self.inner.seek(sequence)
	}

	/// A watch-only handle to the track's subscriber demand.
	pub fn demand(&self) -> moq_net::TrackDemand {
		self.inner.demand()
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
	use crate::container::Timestamp;

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
		let mut broadcast = moq_net::Broadcast::new().produce();
		let catalog = crate::catalog::Producer::new(&mut broadcast).unwrap();
		(broadcast, catalog)
	}

	#[tokio::test(start_paused = true)]
	async fn existing_track_opus_uses_existing_name() {
		let (mut broadcast, catalog) = new_broadcast();
		// The importer accepts the reserved track, setting its (microsecond) timescale.
		let request = broadcast.create_track(moq_net::Track::new("requested-audio")).unwrap();
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
		let request = broadcast.create_track(moq_net::Track::new(name)).unwrap();
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
		let track = broadcast.create_track(moq_net::Track::new("audio")).unwrap();
		let subscriber = track.consume();

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
		let request = broadcast.create_track(moq_net::Track::new("camera")).unwrap();

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
		let request = broadcast.create_track(moq_net::Track::new("video")).unwrap();
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
