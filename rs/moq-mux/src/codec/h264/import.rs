//! H.264 importer.
//!
//! [`Import`] publishes already-split H.264 frames on a single moq track and
//! resolves the catalog rendition. It is a pure frame publisher: byte parsing
//! and framing live in [`Split`](super::Split), and whoever drives the import owns the split.
//! Frames arrive via [`decode`](Import::decode).
//!
//! The codec config comes from exactly one of two places: an avcC handed to
//! [`initialize`](Import::initialize) (the "avc1" shape), or the SPS the splitter
//! packages into the first keyframe (the "avc3" shape, scanned out of the frame
//! here). A keyframe that can't be configured from either is an error;
//! non-keyframes before the first config are tolerated (mid-stream joins).

use bytes::Bytes;

use super::{Error, NAL_TYPE_SEI, NAL_TYPE_SPS, Sps};
use crate::Result;
use crate::catalog::hang::CatalogExt;
use crate::codec::annexb::NalIterator;
use crate::container::Frame;

/// H.264 importer: a pure frame publisher that resolves the catalog rendition.
///
/// Build it with [`new`](Self::new), passing the track producer and the
/// [`catalog::Reserved`](crate::catalog::Reserved) it reserves its rendition from.
/// Feed it frames a [`Split`](super::Split) produced via [`decode`](Self::decode).
/// The catalog rendition fills in lazily once the codec config is known (avcC via
/// [`initialize`](Self::initialize) for avc1, the first SPS for avc3).
pub struct Import {
	/// The NALU length size for the avc1 shape, whose codec config is out-of-band (avcC), so
	/// keyframes are not scanned for an inline SPS. `None` for Annex-B.
	length_size: Option<usize>,
	track: crate::container::Producer<crate::catalog::hang::Container, hang::catalog::VideoConfig>,
	catalog: crate::codec::video::Catalog,
	last_sps: Option<Bytes>,
}

impl Import {
	/// Publish on an existing track producer, seeding the rendition from `hint` (pass
	/// [`VideoHint::default`](crate::catalog::VideoHint) for none).
	///
	/// A hint carrying a codec publishes the catalog rendition up front (the SPS/PPS still refine it
	/// in band on the first keyframe), so subscribers see the track without waiting for that frame.
	pub fn new<E: CatalogExt>(
		track: moq_net::track::Producer,
		reserved: crate::catalog::Reserved<E>,
		hint: crate::catalog::VideoHint,
	) -> crate::Result<Self> {
		// The hint names the container; the writer is built from that same value so the wire
		// cannot disagree with what the rendition advertises.
		let wire = crate::catalog::hang::Container::try_from(&hint)?;
		let catalog = crate::codec::video::Catalog::new(hint);
		let track = reserved.video(track, wire, None)?;
		let mut import = Self {
			length_size: None,
			track,
			catalog,
			last_sps: None,
		};
		if let Some(config) = import.catalog.initial_config() {
			import.apply_config(config)?;
		}
		Ok(import)
	}

	/// Resolve the codec config from the codec's leading bytes.
	///
	/// - **avc1** (no leading start code): parsed as an `AVCDecoderConfigurationRecord`,
	///   which resolves the config and is stored as the catalog `description`. Required
	///   for avc1.
	/// - **avc3** (leading start code): parsed as Annex-B; any SPS resolves the config.
	///   Optional, since avc3 also self-initializes from the first keyframe.
	///
	/// Takes a read-only slice: the dispatcher-owned [`Split`](super::Split) is what
	/// consumes the stream (and reads the same avcC for the NALU length size). The
	/// shape is detected from the leading bytes.
	pub fn initialize(&mut self, buf: &[u8]) -> Result<()> {
		if crate::codec::annexb::is_config_record(buf) {
			self.initialize_avc1(buf)
		} else {
			self.initialize_avc3(buf)
		}
	}

	fn initialize_avc1(&mut self, avcc_bytes: &[u8]) -> Result<()> {
		// Only switch to avc1 mode once the avcC actually parses, so a parse failure leaves the
		// importer in avc3 mode where inline-SPS keyframes still self-initialize.
		let config = config_from_avcc(avcc_bytes)?;
		// A new record starts a new coded sequence, whose HDR metadata the samples will carry.
		self.catalog.hdr(&mut self.track, Default::default(), true)?;
		self.length_size = Some(super::Avcc::parse(avcc_bytes)?.length_size);
		self.apply_config(config)?;
		Ok(())
	}

	fn initialize_avc3(&mut self, data: &[u8]) -> Result<()> {
		// Resolve the config from any SPS in the seed buffer. Scan a clone so the
		// caller's buffer is left intact for the splitter to consume.
		let mut scan = Bytes::copy_from_slice(data);
		let mut nals = NalIterator::new(&mut scan);
		while let Some(nal) = nals.next().transpose()? {
			if is_sps(&nal) {
				self.seed_sps(&nal)?;
			}
		}
		if let Some(nal) = nals.flush()?
			&& is_sps(&nal)
		{
			self.seed_sps(&nal)?;
		}
		Ok(())
	}

	/// Configure from an SPS in a seed buffer. A new one starts a coded sequence, so the last one's
	/// HDR metadata is dropped before the config publishes; the seed's own SEI arrives with its
	/// keyframe through the splitter.
	fn seed_sps(&mut self, sps: &Bytes) -> Result<()> {
		if self.last_sps.as_ref() != Some(sps) {
			self.catalog.hdr(&mut self.track, Default::default(), true)?;
		}
		self.configure_from_sps(sps)?;
		Ok(())
	}

	/// The MoQ track name this importer publishes on, which a decoder identity change replaces
	/// with a newly minted one.
	pub fn name(&self) -> &str {
		self.track.name()
	}

	/// A watch-only handle to this track's subscriber demand.
	pub fn demand(&self) -> moq_net::track::Demand {
		self.track.track().demand()
	}

	/// The track this importer publishes on, which a decoder identity change replaces with a
	/// newly named one.
	pub fn track(&self) -> &moq_net::track::Producer {
		self.track.track()
	}

	/// Finish the track, flushing any buffered data.
	pub fn finish(&mut self) -> Result<()> {
		self.track.finish()?;
		Ok(())
	}

	/// Abort the track with `err` instead of finishing it cleanly, so subscribers
	/// see the real cause rather than [`moq_net::Error::Dropped`]. Consumes this importer.
	pub fn abort(self, err: moq_net::Error) {
		self.track.abort(err);
	}

	/// Cut the current group at `end` without finishing the track.
	pub fn cut(&mut self, end: Option<moq_net::Timestamp>) -> Result<()> {
		self.track.cut(end)?;
		Ok(())
	}

	/// Mark a break in the timeline by publishing a marker group. To bound the closing
	/// group's final frame first, [`cut(end)`](Self::cut) before this. See
	/// [`Producer::discontinuity`](crate::container::Producer::discontinuity).
	pub fn discontinuity(&mut self) -> Result<()> {
		self.track.discontinuity()?;
		Ok(())
	}

	/// Advertise whether this rendition is being produced, as the catalog's
	/// [`enabled`](hang::catalog::VideoConfig::enabled): a publisher that stops encoding it
	/// disables it, so viewers select another rendition rather than wait for frames.
	pub fn set_enabled(&mut self, enabled: bool) -> Result<()> {
		self.catalog.set_enabled(&mut self.track, enabled)?;
		Ok(())
	}

	/// Close the current group and open the next one at `sequence`.
	pub fn seek(&mut self, sequence: u64) -> Result<()> {
		self.track.seek(sequence)?;
		Ok(())
	}

	/// Record a frame's reorder delay (`PTS - DTS`) so the catalog `jitter` reflects the
	/// B-frame reorder depth (the decode buffer a transmuxer/player must hold). The
	/// container supplies this since the elementary stream alone carries no decode time.
	pub fn observe_reorder(&mut self, reorder: moq_net::Timestamp) -> crate::Result<()> {
		self.track.reorder(reorder);
		Ok(())
	}

	/// Resolve the avc3 config from an inline SPS, updating it in place.
	///
	/// avc3 carries SPS inline, so a resolution change just updates the config
	/// (no new init segment, unlike avc1).
	fn configure_from_sps(&mut self, sps_nal: &Bytes) -> Result<()> {
		if self.last_sps.as_ref() == Some(sps_nal) {
			return Ok(());
		}
		let config = config_from_sps(sps_nal)?;
		// Own just the parameter set: the access unit it was found in is far larger.
		self.last_sps = Some(Bytes::copy_from_slice(sps_nal));
		self.apply_config(config)?;
		Ok(())
	}

	/// Apply a resolved config, minting a track when the decoder identity changes.
	fn apply_config(&mut self, config: hang::catalog::VideoConfig) -> crate::Result<()> {
		self.catalog.publish(&mut self.track, config)
	}

	/// Write split frames to the track, resolving the avc3 config from the first
	/// keyframe's inline SPS and refining the catalog jitter as it goes.
	fn write_frames(&mut self, frames: impl IntoIterator<Item = Frame>) -> Result<()> {
		for frame in frames {
			if frame.keyframe {
				let hdr =
					crate::codec::color::access_unit(&frame.payload, self.length_size, 1, |h| h & 0x1f == NAL_TYPE_SEI);
				// avc1 config arrives out-of-band via initialize(); avc3 carries the SPS inline.
				let sps = self.length_size.is_none().then(|| find_sps(&frame.payload)).flatten();
				// A new SPS starts a coded sequence whose HDR metadata replaces the last one's. It goes
				// first, so a keyframe that also configures publishes once.
				let sequence = sps.as_ref().is_some_and(|sps| self.last_sps.as_ref() != Some(sps));
				self.catalog.hdr(&mut self.track, hdr, sequence)?;
				if let Some(sps) = sps {
					self.configure_from_sps(&sps)?;
				}
			}

			if !self.catalog.configured() {
				// A keyframe we still can't configure is undecodable, so bail
				// loudly. A non-keyframe before config is a mid-stream-join
				// leftover: write it through, and the producer reports
				// MissingKeyframe (which a mid-stream join skips).
				if frame.keyframe {
					return Err(Error::NotInitialized.into());
				}
			}

			self.track.write(frame)?;
		}
		Ok(())
	}

	/// Publish split frames, resolving the avc3 config from the first keyframe's
	/// inline SPS and refining the catalog jitter as it goes.
	pub fn decode(&mut self, frames: impl IntoIterator<Item = Frame>) -> Result<()> {
		self.write_frames(frames)
	}
}

/// The catalog config a stream's codec bytes describe, resolved without publishing anything: an
/// `AVCDecoderConfigurationRecord` (the avc1 shape) or Annex-B carrying an inline SPS (avc3).
///
/// The video counterpart of [`opus::config`](crate::codec::opus::config), for a caller holding the
/// bytes but no track: a publisher that wants its catalog rendition to say what its encoder really
/// emits reads it out of one encoded keyframe rather than guessing.
pub fn config(buf: &[u8]) -> Result<hang::catalog::VideoConfig> {
	if crate::codec::annexb::is_config_record(buf) {
		return config_from_avcc(buf);
	}
	let sps = find_sps(buf).ok_or(Error::NotInitialized)?;
	config_from_sps(&sps)
}

/// The config an `AVCDecoderConfigurationRecord` describes, stored verbatim as the `description`
/// (`inline: false`, the avc1 shape).
pub(crate) fn config_from_avcc(avcc_bytes: &[u8]) -> Result<hang::catalog::VideoConfig> {
	let avcc = super::Avcc::parse(avcc_bytes)?;

	let mut config = hang::catalog::VideoConfig::new(hang::catalog::H264 {
		profile: avcc.profile,
		constraints: avcc.constraints,
		level: avcc.level,
		inline: false,
	});
	config.coded_width = avcc.coded_width;
	config.coded_height = avcc.coded_height;
	config.interlaced = avcc.sps.first().and_then(|sps| super::sps_interlaced(sps)) == Some(true);
	if let Some(vui) = avcc.sps.first().and_then(|sps| super::sps_vui(sps)) {
		vui.apply(&mut config);
	}
	config.description = Some(Bytes::copy_from_slice(avcc_bytes));
	Ok(config)
}

/// The config an inline SPS describes (`inline: true`, the avc3 shape). No `description`: the
/// parameter sets ride with every keyframe.
fn config_from_sps(sps_nal: &[u8]) -> Result<hang::catalog::VideoConfig> {
	let sps = Sps::parse(sps_nal)?;

	let mut config = hang::catalog::VideoConfig::new(hang::catalog::H264 {
		profile: sps.profile,
		constraints: sps.constraints,
		level: sps.level,
		inline: true,
	});
	config.coded_width = Some(sps.coded_width);
	config.coded_height = Some(sps.coded_height);
	config.interlaced = super::sps_interlaced(sps_nal) == Some(true);
	if let Some(vui) = super::sps_vui(sps_nal) {
		vui.apply(&mut config);
	}
	Ok(config)
}

fn is_sps(nal: &[u8]) -> bool {
	nal.first().is_some_and(|h| h & 0x1f == NAL_TYPE_SPS)
}

/// Find the first SPS NAL in an Annex-B payload, if any.
fn find_sps(payload: &[u8]) -> Option<Bytes> {
	let mut buf = Bytes::copy_from_slice(payload);
	let mut nals = NalIterator::new(&mut buf);
	while let Some(Ok(nal)) = nals.next() {
		if is_sps(&nal) {
			return Some(nal);
		}
	}
	nals.flush().ok().flatten().filter(|nal| is_sps(nal))
}

#[cfg(test)]
mod tests {
	use bytes::BytesMut;

	use super::*;
	use crate::codec::h264::Split;

	fn setup(name: &str) -> (moq_net::track::Producer, crate::catalog::Producer) {
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let catalog = crate::catalog::Producer::new(&mut broadcast, crate::catalog::Config::default()).unwrap();
		let track = broadcast
			.create_track(name, hang::container::track_info(hang::catalog::PRIORITY.video))
			.unwrap();
		(track, catalog)
	}

	/// An HDR10 keyframe publishes its VUI colour and the static HDR metadata its SEI carries.
	#[tokio::test(start_paused = true)]
	async fn avc3_keyframe_publishes_hdr10_color() {
		use crate::codec::h264::fixtures::{PPS, SEI_CONTENT_LIGHT, SEI_MASTERING, SPS_HDR10};

		let idr: &[u8] = &[0x65, 0x88, 0x84, 0x21];
		let mut annexb = BytesMut::new();
		for nal in [SPS_HDR10, PPS, SEI_MASTERING, SEI_CONTENT_LIGHT, idr] {
			annexb.extend_from_slice(&[0, 0, 0, 1]);
			annexb.extend_from_slice(nal);
		}

		let mut split = Split::new();
		let (track, catalog) = setup("video");
		let mut import = Import::new(track, catalog.reserve(), Default::default()).unwrap();
		let pts = moq_net::Timestamp::from_micros(0).unwrap();
		let mut frames = split.decode(&annexb, pts).expect("split keyframe");
		frames.extend(split.flush(pts).expect("flush keyframe"));
		import.decode(frames).expect("decode keyframe");

		let color = catalog.snapshot().video.renditions["video"]
			.color
			.clone()
			.expect("colour");
		assert_eq!(
			(color.primaries, color.transfer, color.matrix, color.range),
			(Some(9), Some(16), Some(9), Some(hang::catalog::Range::Limited))
		);
		let hdr = crate::codec::color::test::hdr10();
		assert_eq!(
			(color.mastering, color.content_light),
			(hdr.mastering, hdr.content_light)
		);
	}

	/// HDR metadata merges across keyframes of one coded sequence, and a new SPS drops it, so an
	/// HDR-to-SDR switch never keeps stale HDR10 values.
	#[tokio::test(start_paused = true)]
	async fn a_new_sps_drops_stale_hdr_metadata() {
		use crate::codec::h264::fixtures::{PPS, SEI_CONTENT_LIGHT, SEI_MASTERING, SPS_BT709, SPS_HDR10};

		let idr: &[u8] = &[0x65, 0x88, 0x84, 0x21];
		let (track, catalog) = setup("video");
		let mut import = Import::new(track, catalog.reserve(), Default::default()).unwrap();
		let mut split = Split::new();
		let mut keyframe = |ms: u64, nals: &[&[u8]]| {
			let mut annexb = BytesMut::new();
			for nal in nals {
				annexb.extend_from_slice(&[0, 0, 0, 1]);
				annexb.extend_from_slice(nal);
			}
			let pts = moq_net::Timestamp::from_millis(ms).unwrap();
			let mut frames = split.decode(&annexb, pts).expect("split keyframe");
			frames.extend(split.flush(pts).expect("flush keyframe"));
			import.decode(frames).expect("decode keyframe");
			catalog.snapshot().video.renditions["video"]
				.color
				.clone()
				.expect("colour")
		};

		let color = keyframe(0, &[SPS_HDR10, PPS, SEI_MASTERING, SEI_CONTENT_LIGHT, idr]);
		assert!(color.mastering.is_some());
		let color = keyframe(40, &[SPS_HDR10, PPS, idr]);
		assert!(color.mastering.is_some(), "the same sequence keeps its metadata");
		let color = keyframe(80, &[SPS_BT709, PPS, idr]);
		assert_eq!(color.primaries, Some(1));
		assert_eq!((color.mastering, color.content_light), (None, None));
	}

	/// A disabled rendition stays disabled across a new SPS, and enabling it republishes.
	#[tokio::test(start_paused = true)]
	async fn a_disabled_rendition_survives_a_new_sps() {
		use crate::codec::h264::fixtures::{PPS, SPS_BT709, SPS_HDR10};

		let idr: &[u8] = &[0x65, 0x88, 0x84, 0x21];
		let (track, catalog) = setup("video");
		let mut import = Import::new(track, catalog.reserve(), Default::default()).unwrap();
		let mut split = Split::new();
		let mut keyframe = |import: &mut Import, ms: u64, sps: &[u8]| {
			let mut annexb = BytesMut::new();
			for nal in [sps, PPS, idr] {
				annexb.extend_from_slice(&[0, 0, 0, 1]);
				annexb.extend_from_slice(nal);
			}
			let pts = moq_net::Timestamp::from_millis(ms).unwrap();
			let mut frames = split.decode(&annexb, pts).expect("split keyframe");
			frames.extend(split.flush(pts).expect("flush keyframe"));
			import.decode(frames).expect("decode keyframe");
		};
		let enabled = || catalog.snapshot().video.renditions["video"].enabled;

		keyframe(&mut import, 0, SPS_HDR10);
		assert!(enabled());
		import.set_enabled(false).unwrap();
		assert!(!enabled());
		keyframe(&mut import, 40, SPS_BT709);
		assert_eq!(
			catalog.snapshot().video.renditions["video"]
				.color
				.as_ref()
				.unwrap()
				.primaries,
			Some(1)
		);
		assert!(!enabled(), "a reconfiguration keeps the rendition disabled");
		import.set_enabled(true).unwrap();
		assert!(enabled());
	}

	/// A frame-only SPS declares its reorder depth, which holds for the track's life: a later SPS
	/// declaring less or none keeps it, and one declaring more is refused. An SPS that changes the
	/// decoder identity mints a new track, which declares its own.
	#[tokio::test(start_paused = true)]
	async fn a_declared_reorder_depth_holds_for_the_track() {
		use crate::codec::h264::fixtures::{PPS, SPS_FIELD, SPS_IPB, SPS_PYRAMID};

		let idr: &[u8] = &[0x65, 0x88, 0x84, 0x21];
		let (deep, catalog) = setup("deep");
		let (shallow, _) = setup("shallow");
		let mut deep = Import::new(deep, catalog.reserve(), Default::default()).unwrap();
		let mut shallow = Import::new(shallow, catalog.reserve(), Default::default()).unwrap();
		let mut split = Split::new();
		let mut keyframe = |import: &mut Import, ms: u64, sps: &[u8]| {
			let mut annexb = BytesMut::new();
			for nal in [sps, PPS, idr] {
				annexb.extend_from_slice(&[0, 0, 0, 1]);
				annexb.extend_from_slice(nal);
			}
			let pts = moq_net::Timestamp::from_millis(ms).unwrap();
			let mut frames = split.decode(&annexb, pts).expect("split keyframe");
			frames.extend(split.flush(pts).expect("flush keyframe"));
			import
				.decode(frames)
				.map(|()| catalog.snapshot().video.renditions[import.name()].reorder)
		};

		assert_eq!(keyframe(&mut deep, 0, SPS_PYRAMID).unwrap(), Some(2));
		assert_eq!(keyframe(&mut deep, 40, SPS_IPB).unwrap(), Some(2));
		assert_eq!(deep.name(), "deep");
		// A field-coded SPS is a new profile, level, and scan: a new track with its own bound.
		assert_eq!(keyframe(&mut deep, 80, SPS_FIELD).unwrap(), None, "fields declare none");
		assert_eq!(deep.name(), "deep/1");
		assert_eq!(keyframe(&mut shallow, 0, SPS_IPB).unwrap(), Some(1));
		assert!(keyframe(&mut shallow, 40, SPS_PYRAMID).is_err());
	}

	/// A new out-of-band avcC starts a new coded sequence, so in-band HDR metadata from the last one
	/// is dropped.
	#[tokio::test(start_paused = true)]
	async fn a_new_avcc_drops_stale_hdr_metadata() {
		use crate::codec::h264::fixtures::{PPS, SEI_CONTENT_LIGHT, SEI_MASTERING, SPS_BT709, SPS_HDR10};

		let avcc = |sps: &'static [u8]| {
			crate::codec::h264::build_avcc(&[Bytes::from_static(sps)], &[Bytes::from_static(PPS)]).unwrap()
		};
		let (track, catalog) = setup("video");
		let mut import = Import::new(track, catalog.reserve(), Default::default()).unwrap();
		import.initialize(&avcc(SPS_HDR10)).unwrap();

		let mut au = Vec::new();
		for nal in [SEI_MASTERING, SEI_CONTENT_LIGHT, &[0x65, 0x88, 0x84, 0x21][..]] {
			au.extend_from_slice(&(nal.len() as u32).to_be_bytes());
			au.extend_from_slice(nal);
		}
		let pts = moq_net::Timestamp::from_micros(0).unwrap();
		import
			.decode([crate::codec::h264::avc1_frame(au, 4, pts).unwrap()])
			.unwrap();
		let color = || catalog.snapshot().video.renditions["video"].color.clone().unwrap();
		assert!(color().mastering.is_some());

		// The new avcC is a new description, so it publishes a new track.
		import.initialize(&avcc(SPS_BT709)).unwrap();
		assert_eq!(import.name(), "video/1");
		let color = || catalog.snapshot().video.renditions["video/1"].color.clone().unwrap();
		assert_eq!(color().primaries, Some(1));
		assert_eq!((color().mastering, color().content_light), (None, None));
	}

	/// Seeding an Annex-B importer with a new SPS starts a new coded sequence, dropping the last
	/// one's HDR metadata before the new config publishes.
	#[tokio::test(start_paused = true)]
	async fn a_new_seed_sps_drops_stale_hdr_metadata() {
		use crate::codec::h264::fixtures::{PPS, SEI_CONTENT_LIGHT, SEI_MASTERING, SPS_BT709, SPS_HDR10};

		let annexb = |nals: &[&[u8]]| {
			let mut buf = BytesMut::new();
			for nal in nals {
				buf.extend_from_slice(&[0, 0, 0, 1]);
				buf.extend_from_slice(nal);
			}
			buf
		};
		let (track, catalog) = setup("video");
		let mut import = Import::new(track, catalog.reserve(), Default::default()).unwrap();
		let mut split = Split::new();
		let pts = moq_net::Timestamp::from_micros(0).unwrap();
		let idr: &[u8] = &[0x65, 0x88, 0x84, 0x21];
		let au = annexb(&[SPS_HDR10, PPS, SEI_MASTERING, SEI_CONTENT_LIGHT, idr]);
		let mut frames = split.decode(&au, pts).unwrap();
		frames.extend(split.flush(pts).unwrap());
		import.decode(frames).unwrap();
		let color = || catalog.snapshot().video.renditions["video"].color.clone().unwrap();
		assert!(color().mastering.is_some());

		import.initialize(&annexb(&[SPS_BT709, PPS])).unwrap();
		assert_eq!(color().primaries, Some(1));
		assert_eq!((color().mastering, color().content_light), (None, None));
	}

	/// The stream outranks the caller's hint, which only fills what the stream leaves unsaid.
	#[tokio::test(start_paused = true)]
	async fn stream_color_outranks_the_hint() {
		use crate::codec::h264::fixtures::{PPS, SPS_BT709};

		let mut hinted = hang::catalog::Color::default();
		hinted.primaries = Some(9);
		hinted.content_light = crate::codec::color::test::hdr10().content_light;
		let hint = crate::catalog::VideoHint {
			color: Some(hinted),
			..Default::default()
		};

		let (track, catalog) = setup("video");
		let mut import = Import::new(track, catalog.reserve(), hint).unwrap();
		let avcc =
			crate::codec::h264::build_avcc(&[Bytes::from_static(SPS_BT709)], &[Bytes::from_static(PPS)]).unwrap();
		import.initialize(&avcc).unwrap();

		let color = catalog.snapshot().video.renditions["video"]
			.color
			.clone()
			.expect("colour");
		assert_eq!(color.primaries, Some(1), "the SPS outranks the hint");
		assert_eq!(color.content_light, crate::codec::color::test::hdr10().content_light);
	}

	#[test]
	fn annexb_resolves_display_aspect() {
		use crate::codec::h264::fixtures;
		for (sps, expected) in [
			(fixtures::SPS_IPB, (None, None)),
			(fixtures::SPS_ANAMORPHIC, (Some(16), Some(9))),
			(fixtures::SPS_EXTENDED_SAR, (Some(28), Some(15))),
		] {
			let mut inline = vec![0, 0, 0, 1];
			inline.extend_from_slice(sps);
			let config = config(&inline).unwrap();
			assert_eq!((config.display_aspect_width, config.display_aspect_height), expected);
		}
	}

	#[test]
	fn avcc_resolves_display_aspect() {
		use crate::codec::h264::fixtures;
		for (sps, expected) in [
			(fixtures::SPS_IPB, (None, None)),
			(fixtures::SPS_ANAMORPHIC, (Some(16), Some(9))),
			(fixtures::SPS_EXTENDED_SAR, (Some(28), Some(15))),
		] {
			let avcc =
				super::super::build_avcc(&[Bytes::from_static(sps)], &[Bytes::from_static(fixtures::PPS)]).unwrap();
			let config = config(&avcc).unwrap();
			assert_eq!((config.display_aspect_width, config.display_aspect_height), expected);
		}
	}

	/// Only a fixed-rate SPS states the frame rate; without `fixed_frame_rate_flag` its tick is a
	/// ceiling, left for the catalog estimator to measure.
	#[test]
	fn config_takes_only_a_fixed_framerate() {
		use crate::codec::h264::fixtures;

		let fixed = super::super::build_avcc(
			&[Bytes::from_static(fixtures::SPS_IPB)],
			&[Bytes::from_static(fixtures::PPS)],
		)
		.unwrap();
		assert_eq!(config(&fixed).unwrap().framerate, Some(25.0));

		let mut inline = vec![0, 0, 0, 1];
		inline.extend_from_slice(fixtures::SPS_IPB_VARIABLE);
		assert_eq!(config(&inline).unwrap().framerate, None);
	}

	/// `frame_mbs_only_flag` 0 marks the rendition interlaced, in both the avc1 and avc3 shapes.
	#[test]
	fn config_marks_field_coding_interlaced() {
		use crate::codec::h264::fixtures;

		for (sps, interlaced) in [(fixtures::SPS_IPB, false), (fixtures::SPS_FIELD, true)] {
			let avcc =
				super::super::build_avcc(&[Bytes::from_static(sps)], &[Bytes::from_static(fixtures::PPS)]).unwrap();
			assert_eq!(config(&avcc).unwrap().interlaced, interlaced);

			let mut inline = vec![0, 0, 0, 1];
			inline.extend_from_slice(sps);
			assert_eq!(config(&inline).unwrap().interlaced, interlaced);
		}
	}

	/// An avcC initializer resolves a config with the avcC stored as `description`.
	#[tokio::test(start_paused = true)]
	async fn initialize_avc1_lands_in_catalog() {
		let sps_nal = [0x67, 0x42, 0xc0, 0x1f];
		let mut avcc = vec![0x01, 0x42, 0xc0, 0x1f, 0xff, 0xe1, 0x00, sps_nal.len() as u8];
		avcc.extend_from_slice(&sps_nal);
		avcc.extend_from_slice(&[0x01, 0x00, 0x04, 0x68, 0xce, 0x3c, 0x80]); // num_pps + pps

		let (track, catalog) = setup("video");
		let mut import = Import::new(track, catalog.reserve(), Default::default()).unwrap();
		// initialize() must not consume the buffer (the split owns the consume).
		let buf = bytes::BytesMut::from(avcc.as_slice());
		import.initialize(&buf).expect("initialize avc1");
		assert_eq!(buf.len(), avcc.len(), "initialize must not consume the buffer");

		let snapshot = catalog.snapshot();
		let cfg = snapshot.video.renditions.get("video").expect("rendition");
		let hang::catalog::VideoCodec::H264(h264) = &cfg.codec else {
			panic!("expected H.264 codec")
		};
		assert!(!h264.inline, "avc1 source should land as inline=false");
		assert_eq!(h264.profile, 0x42);
		assert_eq!(h264.level, 0x1f);
		assert_eq!(cfg.description.as_ref().expect("description").as_ref(), avcc.as_slice());
	}

	/// An avc3 stream self-initializes: the config is resolved from the SPS the
	/// splitter packages into the first keyframe.
	#[tokio::test(start_paused = true)]
	async fn avc3_self_initializes_from_first_keyframe() {
		let sps: &[u8] = &[
			0x67, 0x42, 0xc0, 0x1f, 0xda, 0x01, 0x40, 0x16, 0xe9, 0xb8, 0x08, 0x08, 0x0a, 0x00, 0x00, 0x07, 0xd0, 0x00,
			0x01, 0xd4, 0xc0, 0x80,
		];
		let pps: &[u8] = &[0x68, 0xce, 0x3c, 0x80];
		let idr: &[u8] = &[0x65, 0x88, 0x84, 0x21];

		let mut annexb = BytesMut::new();
		for nal in [sps, pps, idr] {
			annexb.extend_from_slice(&[0, 0, 0, 1]);
			annexb.extend_from_slice(nal);
		}

		let mut split = Split::new();
		let (track, catalog) = setup("video");
		let mut import = Import::new(track, catalog.reserve(), Default::default()).unwrap();
		assert!(
			catalog.snapshot().video.renditions.is_empty(),
			"no config before any frame"
		);

		let pts = moq_net::Timestamp::from_micros(0).unwrap();
		let mut frames = split.decode(&annexb, pts).expect("split keyframe");
		frames.extend(split.flush(pts).expect("flush keyframe"));
		import.decode(frames).expect("decode keyframe");

		let snapshot = catalog.snapshot();
		let h264_cfg = snapshot.video.renditions.get("video").expect("rendition");
		let hang::catalog::VideoCodec::H264(h264) = &h264_cfg.codec else {
			panic!("expected H.264 codec")
		};
		assert!(h264.inline, "avc3 source should land as inline=true");
		assert!(h264_cfg.description.is_none(), "avc3 has no out-of-band description");
		assert_eq!(h264.profile, sps[1]);
		assert_eq!(h264.level, sps[3]);
	}

	/// Open-GOP broadcast H.264 (no IDR, random access via recovery-point SEI)
	/// self-initializes: the splitter flags the recovery-point I-slice AU as a
	/// keyframe, so the importer resolves the rendition from its inline SPS.
	#[tokio::test(start_paused = true)]
	async fn open_gop_self_initializes_from_recovery_point() {
		let sps: &[u8] = &[
			0x67, 0x42, 0xc0, 0x1f, 0xda, 0x01, 0x40, 0x16, 0xe9, 0xb8, 0x08, 0x08, 0x0a, 0x00, 0x00, 0x07, 0xd0, 0x00,
			0x01, 0xd4, 0xc0, 0x80,
		];
		let pps: &[u8] = &[0x68, 0xce, 0x3c, 0x80];
		// recovery-point SEI (payload type 6), then a non-IDR I-slice (type 1).
		let sei: &[u8] = &[0x06, 0x06, 0x02, 0x00, 0x40, 0x80];
		let islice: &[u8] = &[0x61, 0xe0, 0x12, 0x34];

		let mut annexb = BytesMut::new();
		for nal in [sei, sps, pps, islice] {
			annexb.extend_from_slice(&[0, 0, 0, 1]);
			annexb.extend_from_slice(nal);
		}

		let mut split = Split::new();
		let (track, catalog) = setup("video");
		let mut import = Import::new(track, catalog.reserve(), Default::default()).unwrap();

		let pts = moq_net::Timestamp::from_micros(0).unwrap();
		let mut frames = split.decode(&annexb, pts).expect("split open-GOP AU");
		frames.extend(split.flush(pts).expect("flush open-GOP AU"));
		import.decode(frames).expect("decode open-GOP AU");

		let snapshot = catalog.snapshot();
		let h264_cfg = snapshot
			.video
			.renditions
			.get("video")
			.expect("open-GOP stream must resolve a video rendition");
		let hang::catalog::VideoCodec::H264(h264) = &h264_cfg.codec else {
			panic!("expected H.264 codec")
		};
		assert!(h264.inline, "avc3 source should land as inline=true");
		assert_eq!(h264.profile, sps[1]);
		assert_eq!(h264.level, sps[3]);
	}

	/// A keyframe that carries no SPS (and no avcC/seed to fall back on) is
	/// undecodable, so it's a hard error rather than an uncatalogued frame.
	#[tokio::test(start_paused = true)]
	async fn keyframe_without_sps_errors() {
		let idr: &[u8] = &[0x65, 0x88, 0x84, 0x21]; // IDR slice, no inline SPS
		let mut annexb = BytesMut::new();
		annexb.extend_from_slice(&[0, 0, 0, 1]);
		annexb.extend_from_slice(idr);

		let mut split = Split::new();
		let (track, catalog) = setup("video");
		let mut import = Import::new(track, catalog.reserve(), Default::default()).unwrap();

		let pts = moq_net::Timestamp::from_micros(0).unwrap();
		let mut frames = split.decode(&annexb, pts).expect("split keyframe");
		frames.extend(split.flush(pts).expect("flush keyframe"));
		let err = import
			.decode(frames)
			.expect_err("an unconfigurable keyframe must error");
		assert!(matches!(err, crate::Error::H264(Error::NotInitialized)), "got {err:?}");
	}

	/// A non-keyframe before the first keyframe has no group to anchor it, so the
	/// producer surfaces MissingKeyframe (which a mid-stream join skips). It must
	/// not silently abort the import.
	#[tokio::test(start_paused = true)]
	async fn delta_before_init_reports_missing_keyframe() {
		let pslice: &[u8] = &[0x61, 0xe0, 0x12, 0x34]; // non-IDR slice
		let mut annexb = BytesMut::new();
		annexb.extend_from_slice(&[0, 0, 0, 1]);
		annexb.extend_from_slice(pslice);

		let mut split = Split::new();
		let (track, catalog) = setup("video");
		let mut import = Import::new(track, catalog.reserve(), Default::default()).unwrap();

		let pts = moq_net::Timestamp::from_micros(0).unwrap();
		let mut frames = split.decode(&annexb, pts).expect("split delta");
		frames.extend(split.flush(pts).expect("flush delta"));
		let err = import
			.decode(frames)
			.expect_err("a delta before any keyframe must report MissingKeyframe");
		assert!(matches!(err, crate::Error::MissingKeyframe(_)), "got {err:?}");
		assert!(
			catalog.snapshot().video.renditions.is_empty(),
			"no config yet, so no catalog"
		);
	}
}
