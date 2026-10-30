//! H.265 importer.
//!
//! Publishes H.265 frames on a single moq track and resolves the catalog
//! rendition, in either wire shape: Annex-B with inline VPS/SPS/PPS ("hev1"), or
//! length-prefixed NALU with an out-of-band hvcC ("hvc1"). Only single-layer
//! streams are supported (VPS is cached but not parsed).
//!
//! The codec config comes from exactly one of two places: an hvcC handed to
//! [`initialize`](Import::initialize) (the hvc1 shape), or the SPS the splitter
//! packages into the first keyframe (hev1). A keyframe
//! that can't be configured is an error; non-keyframes before the first config
//! are written through to the producer, which reports
//! [`MissingKeyframe`](crate::container::MissingKeyframe) for a mid-stream join.
//! Annex-B byte parsing lives in [`Split`](super::Split); this type is a pure frame publisher
//! that whoever owns the split drives via [`decode`](Import::decode).

use bytes::Bytes;

use super::{Error, split::nal_unit_type};
use crate::Result;
use crate::catalog::hang::CatalogExt;
use crate::codec::annexb::NalIterator;
use crate::container::Frame;

/// A pure-publisher importer for H.265.
/// Only supports single layer streams (VPS is cached but not parsed).
///
/// Build it with [`new`](Self::new), passing the track producer and the
/// [`catalog::Reserved`](crate::catalog::Reserved) it reserves its rendition from, and feed it
/// frames a [`Split`](super::Split) produced via [`decode`](Self::decode). The
/// catalog rendition fills in lazily once the codec config is known (hvcC via
/// [`initialize`](Self::initialize) for hvc1, the first SPS for hev1).
pub struct Import {
	/// The NALU length size for the hvc1 shape, whose codec config is out-of-band (hvcC), so
	/// frame payloads are length-prefixed rather than Annex-B. `None` for Annex-B.
	length_size: Option<usize>,
	track: crate::container::Producer<crate::catalog::hang::Container, hang::catalog::VideoConfig>,
	catalog: crate::codec::video::Catalog,
	last_sps: Option<Bytes>,
}

impl Import {
	/// Publish on an existing track producer, seeding the rendition from `hint` (pass
	/// [`VideoHint::default`](crate::catalog::VideoHint) for none).
	///
	/// A hint carrying a codec publishes the catalog rendition up front (the VPS/SPS/PPS still refine
	/// it in band on the first keyframe).
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
	/// - **hvc1** (no leading start code): parsed as an `HEVCDecoderConfigurationRecord`,
	///   which resolves the config and is stored as the catalog `description`. Required
	///   for hvc1.
	/// - **hev1** (leading start code): parsed as Annex-B; any SPS resolves the config.
	///   Optional, since hev1 also self-initializes from the first keyframe.
	///
	/// Takes a read-only slice: the dispatcher-owned [`Split`](super::Split) is what
	/// consumes the stream (and reads the same hvcC for the NALU length size). The
	/// shape is detected from the leading bytes.
	pub fn initialize(&mut self, buf: &[u8]) -> Result<()> {
		if crate::codec::annexb::is_config_record(buf) {
			self.initialize_hvc1(buf)
		} else {
			self.initialize_hev1(buf)
		}
	}

	fn initialize_hvc1(&mut self, hvcc_bytes: &[u8]) -> Result<()> {
		// Only switch to hvc1 mode once the hvcC actually parses, so a parse failure leaves the
		// importer in hev1 mode where inline-SPS keyframes still self-initialize.
		let config = super::config_from_hvcc(hvcc_bytes)?;
		// A new record starts a new coded sequence, whose HDR metadata the samples will carry.
		self.catalog.hdr(&mut self.track, Default::default(), true)?;
		self.length_size = Some(super::Hvcc::parse(hvcc_bytes)?.length_size);
		self.apply_config(config)?;
		Ok(())
	}

	/// Resolve the config from any SPS in the buffer.
	fn initialize_hev1(&mut self, buf: &[u8]) -> Result<()> {
		let mut scan = Bytes::copy_from_slice(buf);
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

	/// Finish the track, flushing the current group.
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

	/// Resolve the config from an inline SPS, updating the rendition in place on a
	/// change.
	fn configure_from_sps(&mut self, sps_nal: &Bytes) -> Result<()> {
		if self.last_sps.as_ref() == Some(sps_nal) {
			return Ok(());
		}
		let config = super::config_from_sps(sps_nal, true)?;
		// Own just the parameter set: the access unit it was found in is far larger.
		self.last_sps = Some(Bytes::copy_from_slice(sps_nal));
		self.apply_config(config)?;
		Ok(())
	}

	/// Apply a resolved config, minting a track when the decoder identity changes.
	///
	fn apply_config(&mut self, config: hang::catalog::VideoConfig) -> crate::Result<()> {
		self.catalog.publish(&mut self.track, config)
	}

	/// Write split frames to the track, resolving the config from the first
	/// keyframe's inline SPS and refining the catalog jitter as it goes.
	fn write_frames(&mut self, frames: impl IntoIterator<Item = Frame>) -> Result<()> {
		for frame in frames {
			if frame.keyframe {
				let hdr = crate::codec::color::access_unit(&frame.payload, self.length_size, 2, |h| {
					nal_unit_type(h) == scuffle_h265::NALUnitType::PrefixSeiNut
				});
				// hvc1 config arrives out-of-band via initialize(); hev1 carries the SPS inline.
				// Scanning an hvc1 payload as Annex-B would misread a length prefix as a start
				// code (a 322-byte NAL prefixes as `00 00 01 42`, an SPS NAL header).
				let sps = self.length_size.is_none().then(|| find_sps(&frame.payload)).flatten();
				// A new SPS starts a coded sequence whose HDR metadata replaces the last one's. It goes
				// first, so a keyframe that also configures publishes once.
				let sequence = sps.as_ref().is_some_and(|sps| self.last_sps.as_ref() != Some(sps));
				self.catalog.hdr(&mut self.track, hdr, sequence)?;
				if let Some(sps) = sps {
					self.configure_from_sps(&sps)?;
				}
			}

			// A keyframe we still can't configure (no SPS) is undecodable.
			if frame.keyframe && !self.catalog.configured() {
				return Err(Error::MissingSps.into());
			}

			// A pre-keyframe delta has no group to anchor it: the producer returns
			// MissingKeyframe, which the caller (e.g. a TS mid-stream join) skips.
			self.track.write(frame)?;
		}
		Ok(())
	}

	/// Publish split frames, resolving the config from the first keyframe's inline
	/// SPS and refining the catalog jitter as it goes.
	pub fn decode(&mut self, frames: impl IntoIterator<Item = Frame>) -> Result<()> {
		self.write_frames(frames)
	}
}

/// The catalog config a stream's codec bytes describe, resolved without publishing anything: an
/// `HEVCDecoderConfigurationRecord` (the hvc1 shape) or Annex-B carrying an inline SPS (hev1).
///
/// The H.265 counterpart of [`h264::config`](crate::codec::h264::config).
pub fn config(buf: &[u8]) -> Result<hang::catalog::VideoConfig> {
	if crate::codec::annexb::is_config_record(buf) {
		return Ok(super::config_from_hvcc(buf)?);
	}
	let sps = find_sps(buf).ok_or(Error::NotInitialized)?;
	Ok(super::config_from_sps(&sps, true)?)
}

fn is_sps(nal: &[u8]) -> bool {
	nal.first()
		.is_some_and(|h| nal_unit_type(*h) == scuffle_h265::NALUnitType::SpsNut)
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
	use crate::codec::h265::{Split, fixtures};

	/// Seeding an Annex-B importer with a new SPS starts a new coded sequence, dropping the last
	/// one's HDR metadata before the new config publishes.
	#[tokio::test(start_paused = true)]
	async fn a_new_seed_sps_drops_stale_hdr_metadata() {
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
		let idr: &[u8] = &[0x26, 0x01, 0x80, 0xaa]; // IdrWRadl (19)
		let au = annexb(&[
			fixtures::VPS,
			fixtures::SPS_HDR10,
			fixtures::PPS,
			fixtures::SEI_CONTENT_LIGHT,
			fixtures::SEI_MASTERING,
			idr,
		]);
		let mut frames = split.decode(&au, pts).unwrap();
		frames.extend(split.flush(pts).unwrap());
		import.decode(frames).unwrap();
		let color = |name: &str| catalog.snapshot().video.renditions[name].color.clone();
		assert!(color("video").unwrap().mastering.is_some());

		// An SPS that signals no colour: nothing is left once the stale metadata goes. It is also a
		// new profile and level, so it publishes a new track.
		import
			.initialize(&annexb(&[fixtures::VPS, fixtures::SPS, fixtures::PPS]))
			.unwrap();
		assert_eq!(import.name(), "video/1");
		assert_eq!(color("video/1"), None);
	}

	/// An HDR10 keyframe publishes its VUI colour and the static HDR metadata its SEI carries.
	#[tokio::test(start_paused = true)]
	async fn hev1_keyframe_publishes_hdr10_color() {
		let idr: &[u8] = &[0x26, 0x01, 0x80, 0xaa]; // IdrWRadl (19)
		let mut annexb = BytesMut::new();
		for nal in [
			fixtures::VPS,
			fixtures::SPS_HDR10,
			fixtures::PPS,
			fixtures::SEI_CONTENT_LIGHT,
			fixtures::SEI_MASTERING,
			idr,
		] {
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

	fn setup(name: &str) -> (moq_net::track::Producer, crate::catalog::Producer) {
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let catalog = crate::catalog::Producer::new(&mut broadcast, crate::catalog::Config::default()).unwrap();
		let track = broadcast
			.create_track(name, hang::container::track_info(hang::catalog::PRIORITY.video))
			.unwrap();
		(track, catalog)
	}

	/// Only an SPS whose HRD fixes the picture rate states it; the VUI tick alone is a ceiling,
	/// left for the catalog estimator to measure.
	#[test]
	fn config_takes_only_a_fixed_framerate() {
		let mut fixed = vec![0, 0, 0, 1];
		fixed.extend_from_slice(fixtures::SPS_PYRAMID);
		assert_eq!(config(&fixed).unwrap().framerate, Some(25.0));

		let mut ceiling = vec![0, 0, 0, 1];
		ceiling.extend_from_slice(fixtures::SPS);
		assert_eq!(config(&ceiling).unwrap().framerate, None);
	}

	/// The SPS's reorder depth at its highest sub-layer becomes the catalog's declaration.
	#[test]
	fn config_declares_the_sps_reorder_depth() {
		let mut pyramid = vec![0, 0, 0, 1];
		pyramid.extend_from_slice(fixtures::SPS_PYRAMID);
		assert_eq!(config(&pyramid).unwrap().reorder, Some(2));
	}

	/// Square samples carry no display aspect, so the picture is shown at its own size.
	#[test]
	fn square_samples_carry_no_display_aspect() {
		let mut nvenc = vec![0, 0, 0, 1];
		nvenc.extend_from_slice(fixtures::SPS_NVENC);
		let config = config(&nvenc).unwrap();
		assert_eq!((config.coded_width, config.coded_height), (Some(1280), Some(720)));
		assert_eq!(
			(config.display_aspect_width, config.display_aspect_height),
			(None, None)
		);
	}

	/// Non-square samples resolve to the whole picture's aspect: 1440x1080 at 4:3 is shown 16:9.
	#[test]
	fn anamorphic_samples_resolve_to_the_picture_aspect() {
		let mut anamorphic = vec![0, 0, 0, 1];
		anamorphic.extend_from_slice(fixtures::SPS_ANAMORPHIC);
		let config = config(&anamorphic).unwrap();
		assert_eq!((config.coded_width, config.coded_height), (Some(1440), Some(1080)));
		assert_eq!(
			(config.display_aspect_width, config.display_aspect_height),
			(Some(16), Some(9))
		);
	}

	#[test]
	fn hvcc_resolves_display_aspect() {
		for (sps, expected) in [
			(fixtures::SPS_NVENC, (None, None)),
			(fixtures::SPS_ANAMORPHIC, (Some(16), Some(9))),
		] {
			let hvcc = super::super::build_hvcc(
				&[Bytes::from_static(fixtures::VPS)],
				&[Bytes::from_static(sps)],
				&[Bytes::from_static(fixtures::PPS)],
			)
			.unwrap();
			let config = config(&hvcc).unwrap();
			assert_eq!((config.display_aspect_width, config.display_aspect_height), expected);
		}
	}

	/// An hvcC initializer resolves a config with the hvcC stored as `description`.
	/// The hvc1 analogue of the avcC test in [`crate::codec::h264`]'s importer.
	#[tokio::test(start_paused = true)]
	async fn initialize_hvcc_lands_in_catalog() {
		let hvcc = fixtures::hvcc();

		let (track, catalog) = setup("video");
		let mut import = Import::new(track, catalog.reserve(), Default::default()).unwrap();
		// initialize() must not consume the buffer (the dispatcher reads the same
		// hvcC for the NALU length size).
		let buf = BytesMut::from(hvcc.as_ref());
		import.initialize(&buf).expect("initialize hvcC");
		assert_eq!(buf.len(), hvcc.len(), "initialize must not consume the buffer");

		let snapshot = catalog.snapshot();
		let cfg = snapshot.video.renditions.get("video").expect("rendition");
		let hang::catalog::VideoCodec::H265(h265) = &cfg.codec else {
			panic!("expected H.265 codec")
		};
		assert!(!h265.in_band, "hvc1 source should land as in_band=false");
		assert_eq!(cfg.coded_width, Some(1280));
		assert_eq!(cfg.coded_height, Some(720));
		assert_eq!(cfg.description.as_deref(), Some(hvcc.as_ref()));
	}

	/// An hvc1 payload is never scanned for an inline SPS. A NAL of exactly 322 bytes
	/// carries the 4-byte length prefix `00 00 01 42`, which the Annex-B scanner reads
	/// as a 3-byte start code followed by an SPS NAL header (0x42 is type 33), so
	/// scanning would fail the parse and reject a perfectly valid keyframe.
	#[tokio::test(start_paused = true)]
	async fn hvc1_length_prefix_is_not_scanned_as_annexb() {
		let hvcc = fixtures::hvcc();
		let (track, catalog) = setup("video");
		let mut import = Import::new(track, catalog.reserve(), Default::default()).unwrap();
		import.initialize(&hvcc).expect("initialize hvcC");

		let mut nal = vec![0x26, 0x01, 0x80, 0xaa]; // IdrWRadl (19)
		nal.resize(322, 0x80);
		let mut au = Vec::new();
		au.extend_from_slice(&(nal.len() as u32).to_be_bytes());
		au.extend_from_slice(&nal);
		assert_eq!(
			&au[..4],
			&[0, 0, 1, 0x42],
			"the length prefix must look like a start code"
		);

		let pts = moq_net::Timestamp::from_micros(0).unwrap();
		let frame = crate::codec::h265::hvc1_frame(&au, 4, pts).unwrap();
		assert!(frame.keyframe);
		import.decode([frame]).expect("hvc1 keyframe");

		// The out-of-band config survives: a scan would have overwritten it with in_band=true.
		let snapshot = catalog.snapshot();
		let cfg = snapshot.video.renditions.get("video").expect("rendition");
		let hang::catalog::VideoCodec::H265(h265) = &cfg.codec else {
			panic!("expected H.265 codec")
		};
		assert!(!h265.in_band);
		assert_eq!(cfg.description.as_deref(), Some(hvcc.as_ref()));
	}

	/// A hev1 stream self-initializes: the config is resolved from the SPS the
	/// splitter packages into the first keyframe.
	#[tokio::test(start_paused = true)]
	async fn hev1_self_initializes_from_first_keyframe() {
		let idr: &[u8] = &[0x26, 0x01, 0x80, 0xaa]; // IdrWRadl (19)
		let mut annexb = BytesMut::new();
		for nal in [fixtures::VPS, fixtures::SPS, fixtures::PPS, idr] {
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
		let cfg = snapshot.video.renditions.get("video").expect("rendition");
		let hang::catalog::VideoCodec::H265(h265) = &cfg.codec else {
			panic!("expected H.265 codec")
		};
		assert!(h265.in_band, "hev1 source should land as in_band=true");
		assert!(cfg.description.is_none(), "hev1 has no out-of-band description");
		assert_eq!(cfg.coded_width, Some(1280));
		assert_eq!(cfg.coded_height, Some(720));
	}
}
