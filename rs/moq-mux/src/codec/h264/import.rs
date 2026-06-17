//! H.264 importer.
//!
//! [`Import`] publishes already-split H.264 frames on a single moq track and
//! resolves the catalog rendition. It is a pure frame publisher: byte parsing
//! and framing live in [`Split`](super::Split), and whoever drives the import owns the split.
//! Frames arrive via the [`FrameDecode`] trait ([`decode`](FrameDecode::decode)).
//!
//! The codec config comes from exactly one of two places: an avcC handed to
//! [`initialize`](Import::initialize) (the "avc1" shape), or the SPS the splitter
//! packages into the first keyframe (the "avc3" shape, scanned out of the frame
//! here). A keyframe that can't be configured from either is an error;
//! non-keyframes before the first config are tolerated (mid-stream joins).

use bytes::{Buf, Bytes};

use super::{Error, NAL_TYPE_SPS, Sps};
use crate::Result;
use crate::codec::annexb::NalIterator;
use crate::container::Frame;
use crate::container::jitter::MinFrameDuration;
use crate::import::{FrameDecode, Renditions};

/// H.264 importer: a pure frame publisher that resolves the catalog rendition.
///
/// Build it from a [`moq_net::TrackRequest`] ([`new`](Self::new), the on-demand
/// path) or an existing [`moq_net::TrackProducer`] ([`from_track`](Self::from_track),
/// the broadcast-push / fixed-track path). Feed it frames a [`Split`](super::Split) produced via
/// the [`FrameDecode`] impl. The catalog rendition fills in lazily once the codec
/// config is known (avcC via [`initialize`](Self::initialize) for avc1, the first
/// SPS for avc3); read it via [`catalog`](Self::catalog) or attach the importer to
/// a broadcast catalog with [`crate::import::Track`].
pub struct Import {
	/// True for the avc1 shape: the codec config is out-of-band (avcC), so
	/// keyframes are not scanned for an inline SPS.
	avc1: bool,
	track: crate::container::Producer<crate::catalog::hang::Container>,
	catalog: hang::Catalog,
	config: Option<hang::catalog::VideoConfig>,
	last_sps: Option<Bytes>,
	jitter: MinFrameDuration,
}

impl Import {
	/// Serve a track request, accepting it at the microsecond timescale.
	pub fn new(request: moq_net::TrackRequest) -> Self {
		let info = moq_net::TrackInfo::default().with_timescale(hang::container::TIMESCALE);
		Self::from_track(request.accept(info))
	}

	/// Publish on an existing track producer.
	pub fn from_track(track: moq_net::TrackProducer) -> Self {
		Self {
			avc1: false,
			track: crate::container::Producer::new(track, crate::catalog::hang::Container::Legacy),
			catalog: hang::Catalog::default(),
			config: None,
			last_sps: None,
			jitter: MinFrameDuration::new(),
		}
	}

	/// Resolve the codec config from the codec's leading bytes.
	///
	/// - **avc1** (no leading start code): the buffer is parsed as an
	///   `AVCDecoderConfigurationRecord`, which resolves the config and is stored
	///   as the catalog `description`. Required for avc1.
	/// - **avc3** (leading start code): the buffer is parsed as Annex-B; any SPS
	///   resolves the config. Optional, since avc3 also self-initializes from the
	///   first keyframe.
	///
	/// The buffer is *not* consumed: the dispatcher-owned [`Split`](super::Split) consumes it
	/// (and reads the same avcC for the NALU length size). The shape is detected
	/// from the leading bytes.
	pub fn initialize<T: Buf + AsRef<[u8]>>(&mut self, buf: &mut T) -> Result<()> {
		if detect_avc1(buf.as_ref()) {
			self.initialize_avc1(buf.as_ref())
		} else {
			self.initialize_avc3(buf.as_ref())
		}
	}

	fn initialize_avc1(&mut self, avcc_bytes: &[u8]) -> Result<()> {
		self.avc1 = true;
		let avcc = super::Avcc::parse(avcc_bytes)?;

		let mut config = hang::catalog::VideoConfig::new(hang::catalog::H264 {
			profile: avcc.profile,
			constraints: avcc.constraints,
			level: avcc.level,
			inline: false,
		});
		config.coded_width = avcc.coded_width;
		config.coded_height = avcc.coded_height;
		config.description = Some(Bytes::copy_from_slice(avcc_bytes));
		config.container = hang::catalog::Container::Legacy;

		self.apply_config(config);
		Ok(())
	}

	fn initialize_avc3(&mut self, data: &[u8]) -> Result<()> {
		// Resolve the config from any SPS in the seed buffer. Scan a clone so the
		// caller's buffer is left intact for the splitter to consume.
		let mut scan = Bytes::copy_from_slice(data);
		let mut nals = NalIterator::new(&mut scan);
		while let Some(nal) = nals.next().transpose()? {
			if is_sps(&nal) {
				self.configure_from_sps(&nal)?;
			}
		}
		if let Some(nal) = nals.flush()?
			&& is_sps(&nal)
		{
			self.configure_from_sps(&nal)?;
		}
		Ok(())
	}

	/// The standalone catalog once the codec config is known, else `None`.
	pub fn catalog(&self) -> Option<&hang::Catalog> {
		self.config.is_some().then_some(&self.catalog)
	}

	/// The underlying track producer.
	pub fn track(&self) -> &moq_net::TrackProducer {
		self.track.track()
	}

	/// True once the codec config is known and the catalog rendition is published.
	pub fn is_initialized(&self) -> bool {
		self.config.is_some()
	}

	/// Finish the track, flushing any buffered data.
	pub fn finish(&mut self) -> Result<()> {
		self.track.finish()?;
		Ok(())
	}

	/// Close the current group and open the next one at `sequence`.
	pub fn seek(&mut self, sequence: u64) -> Result<()> {
		self.track.seek(sequence)?;
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
		let sps = Sps::parse(sps_nal)?;
		let mut config = hang::catalog::VideoConfig::new(hang::catalog::H264 {
			profile: sps.profile,
			constraints: sps.constraints,
			level: sps.level,
			inline: true,
		});
		config.coded_width = Some(sps.coded_width);
		config.coded_height = Some(sps.coded_height);
		config.container = hang::catalog::Container::Legacy;

		self.last_sps = Some(sps_nal.clone());
		self.apply_config(config);
		Ok(())
	}

	/// Apply a resolved config, updating the catalog rendition in place.
	///
	/// A changed config (new avcC, or a new inline SPS) just re-mirrors the
	/// rendition; there are no fixed tracks to reject a reconfiguration.
	fn apply_config(&mut self, config: hang::catalog::VideoConfig) {
		if self.config.as_ref() == Some(&config) {
			return;
		}
		tracing::debug!(?config, "starting H.264 track");
		self.catalog
			.video
			.renditions
			.insert(self.track.name().to_string(), config.clone());
		self.config = Some(config);
	}

	/// Write split frames to the track, resolving the avc3 config from the first
	/// keyframe's inline SPS and refining the catalog jitter as it goes.
	fn write_frames(&mut self, frames: impl IntoIterator<Item = Frame>) -> Result<()> {
		for frame in frames {
			// avc1 config arrives out-of-band via initialize(); avc3 carries SPS
			// inline on keyframes.
			if !self.avc1
				&& frame.keyframe
				&& let Some(sps) = find_sps(&frame.payload)
			{
				self.configure_from_sps(&sps)?;
			}

			if self.config.is_none() {
				// A keyframe we still can't configure is undecodable, so bail
				// loudly. A non-keyframe before config is a mid-stream-join
				// leftover: write it through, and the producer reports
				// MissingKeyframe (which a mid-stream join skips).
				if frame.keyframe {
					return Err(Error::NotInitialized.into());
				}
			}

			let pts = frame.timestamp;
			self.track.write(frame)?;

			if let Some(jitter) = self.jitter.observe(pts)
				&& let Some(c) = self.catalog.video.renditions.get_mut(self.track.name())
			{
				c.jitter = Some(jitter);
			}
		}
		Ok(())
	}
}

impl FrameDecode for Import {
	fn decode<I: IntoIterator<Item = Frame>>(&mut self, frames: I) -> Result<()> {
		self.write_frames(frames)
	}
}

impl Renditions for Import {
	fn renditions(&self) -> &hang::Catalog {
		&self.catalog
	}
}

/// Detect the avc1 wire shape from leading bytes: a 3- or 4-byte Annex-B start
/// code means avc3, otherwise an AVCDecoderConfigurationRecord (avc1).
fn detect_avc1(bytes: &[u8]) -> bool {
	!(matches!(bytes, [0, 0, 1, ..]) || matches!(bytes, [0, 0, 0, 1, ..]))
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

	fn track(name: &str) -> moq_net::TrackProducer {
		let mut broadcast = moq_net::BroadcastInfo::new().produce();
		broadcast
			.create_track(
				name,
				moq_net::TrackInfo::default().with_timescale(hang::container::TIMESCALE),
			)
			.unwrap()
	}

	/// An avcC initializer resolves a config with the avcC stored as `description`.
	#[tokio::test(start_paused = true)]
	async fn initialize_avc1_lands_in_catalog() {
		let sps_nal = [0x67, 0x42, 0xc0, 0x1f];
		let mut avcc = vec![0x01, 0x42, 0xc0, 0x1f, 0xff, 0xe1, 0x00, sps_nal.len() as u8];
		avcc.extend_from_slice(&sps_nal);
		avcc.extend_from_slice(&[0x01, 0x00, 0x04, 0x68, 0xce, 0x3c, 0x80]); // num_pps + pps

		let mut import = Import::from_track(track("video"));
		// initialize() must not consume the buffer (the split owns the consume).
		let mut buf = bytes::BytesMut::from(avcc.as_slice());
		import.initialize(&mut buf).expect("initialize avc1");
		assert_eq!(buf.len(), avcc.len(), "initialize must not consume the buffer");

		let cfg = import
			.catalog()
			.expect("catalog known after init")
			.video
			.renditions
			.get("video")
			.expect("rendition");
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
		let mut import = Import::from_track(track("video"));
		assert!(import.catalog().is_none(), "no config before any frame");

		let pts = moq_net::Timestamp::from_micros(0).unwrap();
		let mut frames = split.decode(&mut annexb, pts).expect("split keyframe");
		frames.extend(split.flush(pts).expect("flush keyframe"));
		import.decode(frames).expect("decode keyframe");

		let cfg = import.catalog().expect("config after keyframe");
		let h264_cfg = cfg.video.renditions.get("video").expect("rendition");
		let hang::catalog::VideoCodec::H264(h264) = &h264_cfg.codec else {
			panic!("expected H.264 codec")
		};
		assert!(h264.inline, "avc3 source should land as inline=true");
		assert!(h264_cfg.description.is_none(), "avc3 has no out-of-band description");
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
		let mut import = Import::from_track(track("video"));

		let pts = moq_net::Timestamp::from_micros(0).unwrap();
		let mut frames = split.decode(&mut annexb, pts).expect("split keyframe");
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
		let mut import = Import::from_track(track("video"));

		let pts = moq_net::Timestamp::from_micros(0).unwrap();
		let mut frames = split.decode(&mut annexb, pts).expect("split delta");
		frames.extend(split.flush(pts).expect("flush delta"));
		let err = import
			.decode(frames)
			.expect_err("a delta before any keyframe must report MissingKeyframe");
		assert!(matches!(err, crate::Error::MissingKeyframe(_)), "got {err:?}");
		assert!(import.catalog().is_none(), "no config yet, so no catalog");
	}
}
