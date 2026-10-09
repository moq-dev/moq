//! Video decoder front end.
//!
//! Prepares each container frame for a [`Backend`](super::backend::Backend):
//! converts length-prefixed H.264 / H.265 payloads to Annex-B and injects the
//! description's parameter sets ahead of keyframes, leaving Annex-B payloads,
//! AV1 OBU temporal units, and VP8 / VP9 frames untouched. Gates output until the
//! first keyframe so the backend never sees a delta frame it can't decode.
//!
//! The container decides the NAL framing. CMAF samples are always
//! length-prefixed (ISO/IEC 14496-15), so even an avc3 / hev1 track, whose
//! parameter sets ride in the samples, converts with the length size from its
//! description. A Legacy or LOC track is Annex-B when the codec says its
//! parameter sets are in band (avc3 / hev1), and length-prefixed otherwise
//! (avc1 / hvc1).
//!
//! A track that says avc1 and carries no description is read as Annex-B rather
//! than refused. A browser encoding with WebCodecs' `annexb` output keeps the
//! avc1 label while putting its parameter sets in band, which is what
//! `@moq/publish` does today. Length-prefixed payloads without their parameter
//! sets could not be decoded anyway, so the lenient reading only ever turns an
//! error into a picture.

use std::marker::PhantomData;
use std::rc::Rc;

use bytes::Bytes;
use hang::catalog::{AV1, Container, VP9, VideoCodec, VideoConfig};
use moq_mux::codec::{annexb, h264, h265};
use moq_net::Timestamp;

use super::backend::{self, Backend, Codec};
use crate::{Error, Frame, Output, Size, Surface};

/// Which decoder implementation to use. `#[non_exhaustive]` so new selection
/// strategies can be added without breaking external `match`es.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub enum Kind {
	/// Prefer a platform hardware decoder, falling back to enabled software.
	#[default]
	Auto,
	/// Hardware only; error if none is available.
	Hardware,
	/// Software only (OpenH264 and libvpx, when their features are enabled).
	Software,
	/// A specific backend by name, e.g. `"videotoolbox"`, `"mediacodec"`,
	/// `"nvdec"`, `"vaapi"`, `"v4l2"`, `"openh264"`, or `"vpx"`.
	Named(String),
}

/// Decoder configuration: the codec implementation and the frames it hands back.
///
/// Nothing here is about the track: where a [`Consumer`](super::Consumer)
/// starts and how far it may lag live are its [`Options`](super::Options).
///
/// `#[non_exhaustive]`: build via [`Config::new`] (or `default()`) and set the
/// optional fields, so future knobs don't break callers.
#[derive(Clone, Debug, Default)]
#[non_exhaustive]
pub struct Config {
	/// Which backend to use.
	pub kind: Kind,
	/// Where decoded pictures live.
	///
	/// [`Output::Native`] hands back whatever the backend decoded into: a
	/// `CVPixelBuffer` from VideoToolbox, a Direct3D11 texture from Media
	/// Foundation, a CUDA buffer from NVDEC, a DMA-BUF from VAAPI, CPU I420 from
	/// OpenH264. A consumer that draws imports those directly (`render::Renderer`),
	/// and a transcoder re-encodes them in place. [`Output::Cpu`] delivers every
	/// picture as [`Surface::I420`](crate::Surface::I420): a backend that can
	/// decode straight to system memory does, the rest download each picture.
	/// Ask for it when the pixels are headed for the CPU anyway, since a GPU
	/// surface handed out and downloaded later costs an allocation the backend
	/// could have skipped.
	pub output: Output,
	/// Ask the decoder to scale its output to this size (both dimensions even)
	/// instead of the stream's native one.
	///
	/// A hint, not a contract: a hardware decoder with a built-in scaler (NVDEC)
	/// honors it for free, every other backend ignores it, so the frames still
	/// carry whatever size they decoded at. A caller that needs exactly this
	/// size checks each [`Frame::size`](crate::Frame::size) and applies
	/// [`Frame::resize`](crate::Frame::resize) to the rest; the hint only lets a
	/// decoder that can make that a no-op do so.
	pub scale_hint: Option<Size>,
}

impl Config {
	/// A default config: automatic backend selection, native output, no scaling.
	pub fn new() -> Self {
		Self::default()
	}
}

/// How to turn a container payload into a backend access unit.
enum Conversion {
	/// The payload is already in the backend's input framing: Annex-B for a
	/// Legacy or LOC avc3 / hev1, OBU temporal units for AV1, one coded frame for
	/// VP8 / VP9.
	Passthrough,
	/// avc1 / hvc1, and every CMAF H.264 / H.265 track: length-prefixed NALs.
	/// Replace the length prefixes with start codes and prepend `keyframe_prefix`
	/// (the description's parameter sets, empty when they are all in band) ahead
	/// of every keyframe.
	LengthPrefixed { length_size: usize, keyframe_prefix: Bytes },
}

impl Conversion {
	/// The backend access unit for one container payload.
	fn access_unit(&self, payload: &Bytes, keyframe: bool) -> Result<Bytes, Error> {
		match self {
			// Cheap refcount bump; the backend splits codec units off this buffer.
			Self::Passthrough => Ok(payload.clone()),
			Self::LengthPrefixed {
				length_size,
				keyframe_prefix,
			} => {
				let prefix = keyframe.then(|| keyframe_prefix.as_ref());
				Ok(annexb::from_length_prefixed(payload, *length_size, prefix).map_err(moq_mux::Error::from)?)
			}
		}
	}
}

/// Decodes container payloads (the codec bitstream) into raw [`Frame`]s.
///
/// The bring-your-own-payload layer under [`Consumer`](super::Consumer): use it
/// when the frames don't come from a plain track subscription, e.g. a transcoder
/// serving individually fetched groups. Feed it the payload of each container
/// frame in decode order; it converts length-prefixed H.264 / H.265 to Annex-B,
/// passes AV1 OBU temporal units through, and gates output until the first
/// keyframe.
///
/// A decoder is bound to the thread that opens it. Use [`Sink`](super::Sink)
/// when the owner can move between threads.
///
/// ```compile_fail
/// fn move_to_another_thread(decoder: moq_video::decode::Decoder) {
///     std::thread::spawn(move || drop(decoder));
/// }
/// ```
pub struct Decoder {
	backend: Box<dyn Backend>,
	conversion: Conversion,
	output: Output,
	got_keyframe: bool,
	/// Keeps direct use bound to the constructing thread, regardless of backend.
	_thread_bound: PhantomData<Rc<()>>,
}

impl Decoder {
	/// Build a decoder for the catalog's video config. Errors if the codec is
	/// not supported by the native backends.
	pub fn new(catalog: &VideoConfig, config: &Config) -> Result<Self, Error> {
		// CMAF samples are length-prefixed whatever the sample entry; Legacy and LOC
		// carry Annex-B exactly when the parameter sets are in band.
		let cmaf = matches!(catalog.container, Container::Cmaf { .. });
		let (codec, conversion) = match &catalog.codec {
			VideoCodec::H264(h264) => {
				let conversion = match (cmaf || !h264.inline, catalog.description.as_ref()) {
					(false, _) => Conversion::Passthrough,
					(true, Some(avcc)) => {
						let params = h264::Avcc::parse(avcc).map_err(moq_mux::Error::from)?;
						let keyframe_prefix = annexb::build_prefix(params.sps.iter().chain(params.pps.iter()));
						Conversion::LengthPrefixed {
							length_size: params.length_size,
							keyframe_prefix,
						}
					}
					(true, None) if cmaf => {
						return Err(Error::Codec(anyhow::anyhow!(
							"CMAF H.264 track is missing its avcC description"
						)));
					}
					(true, None) => {
						tracing::warn!("avc1 track has no avcC description; reading it as Annex-B");
						Conversion::Passthrough
					}
				};
				(Codec::H264, conversion)
			}
			VideoCodec::H265(h265) => {
				let conversion = if !cmaf && h265.in_band {
					Conversion::Passthrough
				} else {
					let hvcc = catalog.description.as_ref().ok_or_else(|| {
						Error::Codec(anyhow::anyhow!(
							"length-prefixed H.265 track is missing its hvcC description"
						))
					})?;
					let params = h265::Hvcc::parse(hvcc).map_err(moq_mux::Error::from)?;
					let keyframe_prefix =
						annexb::build_prefix(params.vps.iter().chain(params.sps.iter()).chain(params.pps.iter()));
					Conversion::LengthPrefixed {
						length_size: params.length_size,
						keyframe_prefix,
					}
				};
				(Codec::H265, conversion)
			}
			VideoCodec::AV1(av1) if is_supported_av1(av1) => (Codec::Av1, Conversion::Passthrough),
			VideoCodec::VP8 => (Codec::Vp8, Conversion::Passthrough),
			VideoCodec::VP9(vp9) if is_supported_vp9(vp9) => (Codec::Vp9, Conversion::Passthrough),
			other => return Err(Error::UnsupportedCodec(other.to_string())),
		};

		// Refused here for every backend, so a hint no backend reads is still
		// checked rather than silently carried. NV12 output is what every
		// scaler produces, and its chroma is 2x2 subsampled.
		if let Some(size) = config.scale_hint {
			size.validate("decoder scale hint")?;
		}

		let backend = backend::open(codec, config)?;
		tracing::debug!(decoder = backend.name(), "opened video decoder");
		Ok(Self {
			backend,
			conversion,
			output: config.output,
			got_keyframe: false,
			_thread_bound: PhantomData,
		})
	}

	/// The decoder backend name in use, e.g. `"videotoolbox"`.
	pub fn name(&self) -> &str {
		self.backend.name()
	}

	/// Decode one container frame, returning zero or more raw frames. `timestamp` is
	/// this frame's presentation time; it rides through the decoder and comes back on
	/// each output frame, so a reordering decoder (B-frames) stamps every picture
	/// with its own presentation time rather than this access unit's. With no
	/// reordering the two coincide.
	pub fn decode(&mut self, payload: &Bytes, timestamp: Timestamp, keyframe: bool) -> Result<Vec<Frame>, Error> {
		// Wait for the first keyframe: a decoder started mid-GOP can't decode
		// delta frames, and the parameter sets ride along with the keyframe.
		if !self.got_keyframe {
			if !keyframe {
				return Ok(Vec::new());
			}
			self.got_keyframe = true;
		}

		let access_unit = self.conversion.access_unit(payload, keyframe)?;
		let frames = self.backend.decode(access_unit, timestamp, keyframe)?;
		self.deliver(frames)
	}

	/// Return the frames the backend still holds once the stream has ended.
	///
	/// Call this after the last access unit and before dropping the decoder. The
	/// decoder remains reusable and waits for a keyframe before accepting the
	/// next stream.
	pub fn flush(&mut self) -> Result<Vec<Frame>, Error> {
		self.got_keyframe = false;
		let frames = self.backend.flush()?;
		self.deliver(frames)
	}

	/// Put the backend's pictures in the representation [`Config::output`]
	/// asked for.
	///
	/// The one place the choice is enforced, so a backend only has to know
	/// about it when decoding to the CPU directly is cheaper than downloading
	/// afterwards (VAAPI). A surface with no CPU path fails here rather than
	/// arriving GPU-resident at a caller that said it could not take one.
	fn deliver(&self, frames: Vec<Frame>) -> Result<Vec<Frame>, Error> {
		match self.output {
			Output::Native => Ok(frames),
			Output::Cpu => frames
				.into_iter()
				.map(|frame| Ok(Frame::new(Surface::I420(frame.surface.into_i420()?), frame.timestamp)))
				.collect(),
		}
	}
}

fn is_supported_av1(av1: &AV1) -> bool {
	av1.bitdepth == 8 && !av1.mono_chrome && av1.chroma_subsampling_x && av1.chroma_subsampling_y
}

/// Profile 0 is 8-bit 4:2:0, with either chroma siting (`vpcC` 0 or 1).
fn is_supported_vp9(vp9: &VP9) -> bool {
	vp9.profile == 0 && vp9.bit_depth == 8 && vp9.chroma_subsampling <= 1
}

#[cfg(test)]
mod tests {
	#![cfg_attr(not(feature = "openh264"), allow(dead_code, unused_imports))]

	use moq_net::Timestamp;

	use super::backend::{self, Codec, probe};
	use crate::encode::{Config as EncodeConfig, Encoder, Kind as EncodeKind};
	use crate::frame::I420;
	use crate::{Frame, Surface};

	/// The `index`th frame of a flat `size` stream at 30fps, every pixel at RGB
	/// `level`.
	fn flat_frame(index: u64, level: u8, size: crate::Size) -> Frame {
		let rgba = vec![level; size.pixels() as usize * 4];
		let surface = Surface::rgba(&rgba, size).unwrap();
		Frame::new(surface, Timestamp::from_micros(index * 33_333).unwrap())
	}

	/// The `index`th frame of a mid-gray 320x240 stream, at 30fps.
	fn gray_frame(index: u64) -> Frame {
		flat_frame(index, 0x80, gray_size())
	}

	/// Assert a decoded picture is the expected size and looks like the gray frame
	/// we encoded. Mid-gray RGBA (0x80) is a flat picture: BT.601 limited-range
	/// luma near 125 and neutral chroma near 128. Averaging each plane catches
	/// plane swaps, stride bugs, and a misread Y/UV split that a size check misses.
	fn assert_gray(i420: &I420, width: u32, height: u32) {
		assert_eq!(i420.width, width);
		assert_eq!(i420.height, height);
		let luma = (width * height) as usize;
		// Tightly-packed I420: luma + two quarter-size chroma planes.
		assert_eq!(i420.data.len(), luma * 3 / 2);

		let avg = |plane: &[u8]| plane.iter().map(|&b| b as u32).sum::<u32>() / plane.len() as u32;
		let y = avg(&i420.data[..luma]);
		let u = avg(&i420.data[luma..luma + luma / 4]);
		let v = avg(&i420.data[luma + luma / 4..]);
		assert!((110..=140).contains(&y), "luma {y} off for a gray frame");
		assert!((118..=138).contains(&u), "u {u} off for a gray frame");
		assert!((118..=138).contains(&v), "v {v} off for a gray frame");
	}

	/// Encode 10 gray frames with `encoder`, decode them through `decoder`, and
	/// assert each decoded picture round-trips. Keyframe gating is exercised (the
	/// first packet is a keyframe with inline parameter sets).
	fn round_trip(mut encoder: Encoder, mut decoder: Box<dyn backend::Backend>, expect_name: &str) {
		assert_eq!(decoder.name(), expect_name);

		let mut decoded = Vec::new();
		for i in 0..10u64 {
			let keyframe = i == 0;
			if keyframe {
				encoder.cut().unwrap();
			}
			// Distinct, spread-apart timestamps so a round-tripped value is unambiguous.
			for encoded in encoder.encode(&gray_frame(i)).unwrap() {
				decoded.extend(decoder.decode(encoded.payload, encoded.timestamp, keyframe).unwrap());
			}
		}
		decoded.extend(decoder.flush().unwrap());

		assert!(!decoded.is_empty(), "decoder produced no frames");
		for out in &decoded {
			assert_gray(&out.surface.to_i420().unwrap(), 320, 240);
		}

		// The timestamp rides through the codec and comes back on each picture,
		// including any tail released by the drain. It returns in presentation order:
		// strictly increasing and drawn from the values we fed.
		let micros: Vec<u128> = decoded.iter().map(|d| d.timestamp.as_micros()).collect();
		assert!(
			micros.windows(2).all(|w| w[0] < w[1]),
			"decoded timestamps not strictly increasing: {micros:?}"
		);
		assert!(
			micros.iter().all(|&t| t % 33_333 == 0 && t < 333_330),
			"decoded timestamp outside the fed set: {micros:?}"
		);
	}

	/// A decoder config selecting one backend by kind.
	fn decode_config(kind: super::Kind) -> super::Config {
		super::Config {
			kind,
			..super::Config::new()
		}
	}

	#[cfg(feature = "openh264")]
	/// An openh264 (software H.264) encoder for a `size` test stream at 30fps.
	fn h264_software_encoder(size: crate::Size) -> Encoder {
		Encoder::new(&EncodeConfig {
			kind: EncodeKind::Software,
			..EncodeConfig::new(size.width, size.height, crate::Rate::new(30, 1).unwrap())
		})
		.expect("openh264 encoder")
	}

	/// The size the gray test stream is encoded at.
	fn gray_size() -> crate::Size {
		crate::Size::new(320, 240)
	}

	#[test]
	#[cfg(feature = "openh264")]
	fn openh264_round_trip() {
		let decoder = backend::open(Codec::H264, &decode_config(super::Kind::Software)).expect("openh264 decoder");
		round_trip(h264_software_encoder(gray_size()), decoder, "openh264");
	}

	/// A description-less avc1 track from WebCodecs carries Annex-B payloads with
	/// its parameter sets in band, the only framing that can decode without avcC.
	#[test]
	#[cfg(feature = "openh264")]
	fn avc1_without_avcc_decodes_as_annexb() {
		// The catalog shape observed from @moq/publish: `"codec": "avc1.640028"`
		// and no `description`.
		let h264 = hang::catalog::H264 {
			inline: false,
			profile: 0x64,
			constraints: 0x00,
			level: 0x28,
		};
		let catalog = hang::catalog::VideoConfig::new(h264);
		assert_eq!(catalog.codec.to_string(), "avc1.640028");
		assert!(catalog.description.is_none());

		let mut decoder = super::Decoder::new(&catalog, &decode_config(super::Kind::Software))
			.expect("a description-less avc1 track opens rather than erroring");
		assert!(
			matches!(decoder.conversion, super::Conversion::Passthrough),
			"a description-less avc1 track is read as Annex-B"
		);

		// openh264 emits Annex-B access units with SPS/PPS inline ahead of each
		// IDR, which is the bitstream WebCodecs produces in `annexb` format.
		let mut encoder = h264_software_encoder(gray_size());
		let mut decoded = Vec::new();
		for i in 0..5u64 {
			let keyframe = i == 0;
			if keyframe {
				encoder.cut().unwrap();
			}
			for encoded in encoder.encode(&gray_frame(i)).unwrap() {
				assert!(
					encoded.payload.starts_with(&[0, 0, 0, 1]) || encoded.payload.starts_with(&[0, 0, 1]),
					"the test feeds Annex-B, not length-prefixed NALs"
				);
				decoded.extend(decoder.decode(&encoded.payload, encoded.timestamp, keyframe).unwrap());
			}
		}

		assert!(!decoded.is_empty(), "decoder produced no frames");
		for out in &decoded {
			assert_gray(&out.surface.to_i420().unwrap(), 320, 240);
		}
	}

	/// Rewrite an Annex-B access unit as 4-byte length-prefixed NAL units, the
	/// framing of an MP4 sample.
	fn length_prefixed(annexb: &bytes::Bytes) -> bytes::Bytes {
		let mut buf = annexb.clone();
		let mut nals = moq_mux::codec::annexb::NalIterator::new(&mut buf);
		let mut out = Vec::new();
		let mut push = |nal: bytes::Bytes| {
			out.extend_from_slice(&(nal.len() as u32).to_be_bytes());
			out.extend_from_slice(&nal);
		};
		for nal in nals.by_ref() {
			push(nal.unwrap());
		}
		if let Some(nal) = nals.flush().unwrap() {
			push(nal);
		}
		out.into()
	}

	/// A CMAF avc3 track: length-prefixed samples with SPS/PPS in band, under an
	/// avcC that lists none. `inline` says where the parameter sets are, not how
	/// the NAL units are framed, so the decoder still converts to Annex-B.
	#[test]
	#[cfg(feature = "openh264")]
	fn cmaf_avc3_decodes_length_prefixed_samples() {
		let mut catalog = hang::catalog::VideoConfig::new(hang::catalog::H264 {
			inline: true,
			profile: 0x42,
			constraints: 0xc0,
			level: 0x1f,
		});
		catalog.container = hang::catalog::Container::Cmaf {
			init: bytes::Bytes::new(),
		};
		// configurationVersion, profile, compatibility, level, 4-byte lengths, no SPS, no PPS.
		catalog.description = Some(bytes::Bytes::from_static(&[0x01, 0x42, 0xc0, 0x1f, 0xff, 0xe0, 0x00]));

		let mut decoder = super::Decoder::new(&catalog, &decode_config(super::Kind::Software)).unwrap();
		let mut encoder = h264_software_encoder(gray_size());
		let mut decoded = Vec::new();
		for i in 0..5u64 {
			let keyframe = i == 0;
			if keyframe {
				encoder.cut().unwrap();
			}
			for encoded in encoder.encode(&gray_frame(i)).unwrap() {
				let sample = length_prefixed(&encoded.payload);
				decoded.extend(decoder.decode(&sample, encoded.timestamp, keyframe).unwrap());
			}
		}
		decoded.extend(decoder.flush().unwrap());

		assert!(!decoded.is_empty(), "decoder produced no frames");
		for out in &decoded {
			assert_gray(&out.surface.to_i420().unwrap(), 320, 240);
		}
	}

	/// An Annex-B H.265 track exported to fMP4 and imported back is a CMAF hev1
	/// track: length-prefixed samples, parameter sets in band. The decoder reads the
	/// framing from the container and hands the backend start-coded NAL units.
	#[tokio::test]
	async fn hev1_cmaf_round_trip_reaches_the_backend_as_annexb() {
		// One keyframe of a 64x64 HEVC stream from x265.
		const VPS: &[u8] = &[
			0x40, 0x01, 0x0c, 0x01, 0xff, 0xff, 0x02, 0x20, 0x00, 0x00, 0x03, 0x00, 0x90, 0x00, 0x00, 0x03, 0x00, 0x00,
			0x03, 0x00, 0x1e, 0x95, 0x98, 0x09,
		];
		const SPS: &[u8] = &[
			0x42, 0x01, 0x01, 0x02, 0x20, 0x00, 0x00, 0x03, 0x00, 0x90, 0x00, 0x00, 0x03, 0x00, 0x00, 0x03, 0x00, 0x1e,
			0xa0, 0x20, 0x81, 0x04, 0xd9, 0x65, 0x66, 0x92, 0x4c, 0xaf, 0x01, 0x68, 0x08, 0x00, 0x00, 0x03, 0x00, 0x08,
			0x00, 0x00, 0x03, 0x00, 0xf0, 0x40,
		];
		const PPS: &[u8] = &[0x44, 0x01, 0xc1, 0x72, 0xb4, 0x22, 0x40];
		const IDR: &[u8] = &[0x28, 0x01, 0xaf, 0x08, 0x60, 0xf9, 0x2a, 0x5c, 0xf3, 0x65, 0x62, 0xe8];

		let (origin, driver) = moq_net::origin::Producer::new(Default::default());
		tokio::spawn(moq_net::time::run(driver));
		let mut source = origin.publish("test", Default::default()).unwrap();
		let mut source_catalog =
			moq_mux::catalog::Producer::new(&mut source, moq_mux::catalog::Config::default()).unwrap();
		let track = source
			.create_track("video", hang::container::track_info(hang::catalog::PRIORITY.video))
			.unwrap();
		let mut config = hang::catalog::VideoConfig::new(hang::catalog::H265 {
			in_band: true,
			profile_space: 0,
			profile_idc: 2,
			profile_compatibility_flags: [0x20, 0, 0, 0],
			tier_flag: false,
			level_idc: 0x1e,
			constraint_flags: [0x90, 0, 0, 0, 0, 0],
		});
		config.coded_width = Some(64);
		config.coded_height = Some(64);
		source_catalog
			.modify()
			.unwrap()
			.video
			.renditions
			.insert("video".to_string(), config);
		let mut producer = moq_mux::container::Producer::new(
			track,
			moq_mux::catalog::hang::Container::Legacy(moq_mux::container::Kind::Video),
		);
		let mut keyframe = Vec::new();
		for nal in [VPS, SPS, PPS, IDR] {
			keyframe.extend_from_slice(&[0, 0, 0, 1]);
			keyframe.extend_from_slice(nal);
		}
		producer
			.write(moq_mux::container::Frame {
				timestamp: Timestamp::from_micros(0).unwrap(),
				duration: Some(Timestamp::from_micros(33_333).unwrap()),
				payload: keyframe.into(),
				keyframe: true,
			})
			.unwrap();
		producer.finish().unwrap();

		let catalog = moq_mux::catalog::Consumer::<()>::new(&source.consume(), moq_mux::catalog::CatalogFormat::Hang)
			.await
			.unwrap();
		let mut export = moq_mux::container::fmp4::Export::new(moq_mux::Source::new(origin.consume(), "test"), catalog)
			.with_max_delay(std::time::Duration::from_secs(30));
		let init = export.next().await.unwrap().expect("CMAF init");
		let fragment = export.next().await.unwrap().expect("CMAF fragment");

		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let subscriber = broadcast.consume();
		let catalog = moq_mux::catalog::Producer::new(&mut broadcast, moq_mux::catalog::Config::default()).unwrap();
		let mut import = moq_mux::container::fmp4::Import::new(broadcast, catalog.reserve());
		import.decode(&init).unwrap();
		import.decode(&fragment).unwrap();

		let snapshot = catalog.snapshot();
		let (name, config) = snapshot.video.renditions.iter().next().expect("video rendition");
		let hang::catalog::VideoCodec::H265(h265) = &config.codec else {
			panic!("expected H.265, got {}", config.codec);
		};
		assert!(h265.in_band, "the export writes hev1");
		assert!(matches!(config.container, hang::catalog::Container::Cmaf { .. }));

		let track = subscriber
			.track(name)
			.unwrap()
			.subscribe(moq_net::track::Subscription::default().with_max_delay(std::time::Duration::from_secs(30)))
			.await
			.unwrap();
		let mut track =
			moq_mux::container::Consumer::new(track, moq_mux::catalog::hang::Container::try_from(config).unwrap());
		let sample = track.read().await.unwrap().expect("an imported sample");
		assert!(sample.keyframe);

		let decoder =
			super::Decoder::new(config, &decode_config(super::Kind::Named(probe::BUFFERED_NAME.into()))).unwrap();
		let access_unit = decoder
			.conversion
			.access_unit(&sample.payload, sample.keyframe)
			.unwrap();
		let mut buf = access_unit.clone();
		let mut nals = moq_mux::codec::annexb::NalIterator::new(&mut buf);
		let mut types = Vec::new();
		for nal in nals.by_ref() {
			types.push(nal.unwrap()[0] >> 1);
		}
		types.extend(nals.flush().unwrap().map(|nal| nal[0] >> 1));
		// VPS (32), SPS (33), PPS (34), then the IDR_W_RADL slice (20): every NAL
		// found by its start code, and the parameter sets ahead of the slice.
		assert_eq!(types.first(), Some(&32), "{types:?}");
		assert!(types.contains(&33) && types.contains(&34), "{types:?}");
		assert_eq!(types.last(), Some(&20), "{types:?}");
	}

	/// An inline-H.264 catalog the test probes accept.
	fn probe_catalog() -> hang::catalog::VideoConfig {
		hang::catalog::VideoConfig::new(hang::catalog::H264 {
			inline: true,
			profile: 0x42,
			constraints: 0,
			level: 30,
		})
	}

	/// The native probe opened through the front end with `output` and
	/// `scale_hint`, and the frame it decodes one access unit to.
	fn decode_native(output: crate::Output, scale_hint: Option<crate::Size>) -> Frame {
		let config = super::Config {
			kind: super::Kind::Named(probe::NATIVE_NAME.into()),
			output,
			scale_hint,
		};
		let mut decoder = super::Decoder::new(&probe_catalog(), &config).expect("the native probe opens");
		let mut frames = decoder
			.decode(
				&bytes::Bytes::from_static(b"access unit"),
				Timestamp::from_micros(0).unwrap(),
				true,
			)
			.unwrap();
		assert_eq!(frames.len(), 1, "the probe decodes one picture per access unit");
		frames.pop().unwrap()
	}

	/// The output choice and the scale hint reach the backend as configured,
	/// through the same front end every consumer opens through.
	#[test]
	fn output_and_scale_hint_reach_the_backend() {
		let _probe = probe::native_exclusive();
		let hint = crate::Size::new(160, 120);
		decode_native(crate::Output::Cpu, Some(hint));

		let opened = probe::native_opened().expect("the backend recorded its config");
		assert_eq!(opened.output, crate::Output::Cpu);
		assert_eq!(opened.scale_hint, Some(hint));
	}

	/// CPU output is enforced by the front end, so a backend that hands back
	/// its native surface still delivers I420, and native output leaves the
	/// surface alone.
	///
	/// Only Apple platforms can build a native surface without a device, so this is
	/// where the conversion is exercised; elsewhere the probe's pictures are
	/// already CPU pixels and the assertion pins that native output does not
	/// invent a download.
	#[test]
	fn cpu_output_converts_native_frames() {
		let _probe = probe::native_exclusive();
		let cpu = decode_native(crate::Output::Cpu, None);
		assert!(
			matches!(cpu.surface, Surface::I420(_)),
			"CPU output delivered a native surface"
		);
		assert_eq!(cpu.size(), probe::SIZE);

		let native = decode_native(crate::Output::Native, None);
		#[cfg(apple)]
		assert!(
			matches!(native.surface, Surface::PixelBuffer(_)),
			"native output downloaded the picture"
		);
		#[cfg(not(apple))]
		assert!(matches!(native.surface, Surface::I420(_)));
	}

	/// The scale hint is only a hint: a backend without a scaler decodes at the
	/// stream's size, and the exact size comes from an explicit resize.
	#[test]
	fn scale_hint_is_not_enforced() {
		let _probe = probe::native_exclusive();
		let target = crate::Size::new(160, 120);
		let frame = decode_native(crate::Output::Cpu, Some(target));
		assert_eq!(frame.size(), probe::SIZE, "the front end scaled behind the backend");

		let resized = frame.resize(target, &crate::resize::Config::default()).unwrap();
		assert_eq!(resized.size(), target);
	}

	/// A hint no backend could honor is refused when the decoder opens, for
	/// every backend, rather than carried by the ones that ignore it.
	#[test]
	fn odd_scale_hint_is_refused() {
		let _probe = probe::native_exclusive();
		let config = super::Config {
			kind: super::Kind::Named(probe::NATIVE_NAME.into()),
			scale_hint: Some(crate::Size::new(161, 121)),
			..super::Config::new()
		};
		let Err(err) = super::Decoder::new(&probe_catalog(), &config) else {
			panic!("an odd scale hint opened a decoder");
		};
		assert!(
			!matches!(err, crate::Error::NoDecoder(_)),
			"refused for the wrong reason: {err}"
		);
		assert!(
			probe::native_opened().is_none(),
			"the backend was opened before the hint was checked"
		);
	}

	#[test]
	fn av1_is_supported_by_hardware_only() {
		let catalog = hang::catalog::VideoConfig::new(hang::catalog::AV1::default());
		let config = decode_config(super::Kind::Software);
		let Err(err) = super::Decoder::new(&catalog, &config) else {
			panic!("software AV1 decode unexpectedly opened");
		};
		assert!(matches!(err, crate::Error::NoDecoder(_)));
	}

	#[test]
	fn av1_rejects_unsupported_catalog_shape() {
		let av1 = hang::catalog::AV1 {
			bitdepth: 10,
			..hang::catalog::AV1::default()
		};
		let catalog = hang::catalog::VideoConfig::new(av1);
		let config = decode_config(super::Kind::Auto);
		let Err(err) = super::Decoder::new(&catalog, &config) else {
			panic!("10-bit AV1 decode unexpectedly opened");
		};
		assert!(matches!(err, crate::Error::UnsupportedCodec(_)));
	}

	#[cfg(all(apple, feature = "openh264"))]
	#[test]
	fn videotoolbox_round_trip() {
		let decoder = backend::open(Codec::H264, &decode_config(super::Kind::Named("videotoolbox".into())))
			.expect("videotoolbox decoder");
		round_trip(h264_software_encoder(gray_size()), decoder, "videotoolbox");
	}

	/// Encode `count` gray frames and decode them, returning the decoded pictures.
	/// The shared setup for the residency and re-encode tests below.
	#[cfg(all(apple, feature = "openh264"))]
	fn decode_gray(count: u64) -> Vec<Frame> {
		let mut encoder = h264_software_encoder(gray_size());
		let mut decoder = backend::open(Codec::H264, &decode_config(super::Kind::Named("videotoolbox".into())))
			.expect("videotoolbox decoder");

		let mut decoded = Vec::new();
		for i in 0..count {
			let keyframe = i == 0;
			if keyframe {
				encoder.cut().unwrap();
			}
			for encoded in encoder.encode(&gray_frame(i)).unwrap() {
				decoded.extend(decoder.decode(encoded.payload, encoded.timestamp, keyframe).unwrap());
			}
		}

		assert!(!decoded.is_empty(), "decoder produced no frames");
		decoded
	}

	/// VideoToolbox hands back its `CVPixelBuffer` rather than packing to I420 in
	/// the output callback, which is what leaves a render or re-encode path free of
	/// a CPU round trip. `round_trip` above only checks the pixels, so it passes
	/// either way: this is the test that pins the frame's residency.
	#[cfg(all(apple, feature = "openh264"))]
	#[test]
	fn videotoolbox_decode_stays_gpu_resident() {
		for out in &decode_gray(3) {
			assert!(
				matches!(out.surface, Surface::PixelBuffer(_)),
				"VideoToolbox decode downloaded to the CPU instead of keeping its surface"
			);
		}
	}

	/// The multi-rung transcode path stays on hardware through decode, resize, and
	/// encode. The residency assertion catches a CPU fallback even when the pixels
	/// and dimensions still look right.
	#[cfg(all(apple, feature = "openh264"))]
	#[test]
	fn videotoolbox_resized_surface_reencodes_in_place() {
		let decoded = decode_gray(3);
		let resized: Vec<_> = decoded
			.iter()
			.map(|frame| {
				frame
					.resize(crate::Size::new(160, 120), &crate::resize::Config::default())
					.unwrap()
			})
			.collect();
		for frame in &resized {
			assert_eq!(frame.size(), crate::Size::new(160, 120));
			assert!(
				matches!(frame.surface, Surface::PixelBuffer(_)),
				"VideoToolbox resize downloaded to the CPU"
			);
		}

		let encoder = Encoder::new(&EncodeConfig {
			kind: EncodeKind::Named("videotoolbox".into()),
			..EncodeConfig::new(160, 120, crate::Rate::new(30, 1).unwrap())
		});
		let Ok(mut encoder) = encoder else {
			eprintln!("skipping: no VideoToolbox H.264 hardware encoder available");
			return;
		};

		let mut packets = 0;
		for (i, out) in resized.iter().enumerate() {
			if i == 0 {
				encoder.cut().unwrap();
			}
			packets += encoder.encode(out).unwrap().len();
		}
		packets += encoder.finish().unwrap().len();

		assert!(packets > 0, "re-encoding decoded surfaces produced no packets");
	}

	/// H.265 has no software path, so the HEVC round-trip rides VideoToolbox on
	/// both ends: hardware HEVC encode emitting hev1 (inline VPS/SPS/PPS) and
	/// hardware HEVC decode. Skips cleanly on a Mac without HEVC hardware (older
	/// Intel models predating the HEVC encoder).
	#[cfg(apple)]
	#[test]
	fn videotoolbox_hevc_round_trip() {
		let encoder = Encoder::new(&EncodeConfig {
			kind: EncodeKind::Named("videotoolbox".into()),
			codec: crate::encode::Codec::H265,
			..EncodeConfig::new(320, 240, crate::Rate::new(30, 1).unwrap())
		});
		let Ok(encoder) = encoder else {
			eprintln!("skipping: no VideoToolbox H.265 hardware encoder available");
			return;
		};
		let decoder = backend::open(Codec::H265, &decode_config(super::Kind::Named("videotoolbox".into())))
			.expect("videotoolbox H.265 decoder");
		round_trip(encoder, decoder, "videotoolbox");
	}

	#[cfg(all(target_os = "windows", feature = "openh264"))]
	#[test]
	fn mediafoundation_round_trip() {
		// Requires a hardware decoder MFT (GPU). Skip on machines without one
		// rather than fail: CI runners are often headless.
		let Ok(decoder) = backend::open(
			Codec::H264,
			&decode_config(super::Kind::Named("mediafoundation".into())),
		) else {
			eprintln!("skipping: no Media Foundation H.264 hardware decoder available");
			return;
		};
		round_trip(h264_software_encoder(gray_size()), decoder, "mediafoundation");
	}

	/// A distinct RGB level per frame index, so a caller holding several decoded
	/// pictures at once can tell them apart. Spaced far enough apart that lossy
	/// coding can't blur two of them together, which caps how long a stream this
	/// builds.
	#[cfg(all(target_os = "windows", feature = "openh264"))]
	fn level(index: u64) -> u8 {
		u8::try_from(0x20 + index * 0x10).expect("test stream is short enough to keep its levels distinct")
	}

	/// The limited-range BT.601 luma a flat [`level`] frame decodes to.
	#[cfg(all(target_os = "windows", feature = "openh264"))]
	fn expected_luma(level: u8) -> u32 {
		16 + (219 * level as u32) / 255
	}

	/// Decode `count` frames of a `size` [`level`] stream through the Media
	/// Foundation hardware decoder, holding every picture rather than consuming it
	/// as it arrives. `None` when this machine has no hardware decoder.
	#[cfg(all(target_os = "windows", feature = "openh264"))]
	fn decode_levels(count: u64, size: crate::Size) -> Option<(Vec<Frame>, Box<dyn backend::Backend>)> {
		let mut encoder = h264_software_encoder(size);
		let decoder = backend::open(
			Codec::H264,
			&decode_config(super::Kind::Named("mediafoundation".into())),
		);
		let Ok(mut decoder) = decoder else {
			eprintln!("skipping: no Media Foundation H.264 hardware decoder available");
			return None;
		};

		let mut decoded = Vec::new();
		for i in 0..count {
			let keyframe = i == 0;
			if keyframe {
				encoder.cut().unwrap();
			}
			for encoded in encoder.encode(&flat_frame(i, level(i), size)).unwrap() {
				decoded.extend(decoder.decode(encoded.payload, encoded.timestamp, keyframe).unwrap());
			}
		}

		assert!(!decoded.is_empty(), "decoder produced no frames");
		// The decoder goes back to the caller rather than being dropped here: its
		// `ComGuard` tears Media Foundation down for the whole thread, and a test
		// that keeps working with the frames afterwards would be doing so in a
		// process no application resembles.
		Some((decoded, decoder))
	}

	/// Every plane of a decoded flat frame: its average luma, and its average U and
	/// V, which stay neutral because the source is gray. Chroma is the half that
	/// catches a bad plane split, since the UV plane sits after the *texture's* luma
	/// rows rather than the frame's.
	#[cfg(all(target_os = "windows", feature = "openh264"))]
	fn plane_averages(frame: &Frame) -> (u32, u32, u32) {
		let i420 = frame.surface.to_i420().unwrap();
		let average = |plane: &[u8]| plane.iter().map(|&b| b as u32).sum::<u32>() / plane.len() as u32;
		(average(i420.y()), average(i420.u()), average(i420.v()))
	}

	/// A decoded frame comes back as a GPU texture rather than downloaded pixels,
	/// which is what leaves a render or re-encode path free of a CPU round trip.
	/// `round_trip` above only checks the pixels, so it passes either way: this is
	/// the test that pins the frame's residency.
	#[cfg(all(target_os = "windows", feature = "openh264"))]
	#[test]
	fn mediafoundation_decode_stays_gpu_resident() {
		let Some((decoded, _decoder)) = decode_levels(3, gray_size()) else {
			return;
		};
		for out in &decoded {
			assert!(
				matches!(out.surface, Surface::Texture(_)),
				"Media Foundation decode downloaded to the CPU instead of keeping its picture on the GPU"
			);
		}
	}

	/// Held frames keep their own pixels. The decoder decodes into a short array of
	/// picture buffers and recycles a slice as soon as its sample is released, so
	/// handing that slice out as the frame would let later pictures overwrite
	/// frames a consumer is still holding: the decoder's texture has to be copied
	/// into one of ours on the way out.
	///
	/// A distinct level per frame is what makes that visible; a fixed test picture
	/// looks identical either way.
	#[cfg(all(target_os = "windows", feature = "openh264"))]
	#[test]
	fn mediafoundation_held_frames_keep_their_pixels() {
		// More frames than the decoder's pool has slices (8 on the hardware this
		// was written against, one per picture), so it has to recycle the slices
		// the earliest frames came out of.
		let Some((decoded, _decoder)) = decode_levels(12, gray_size()) else {
			return;
		};

		for (i, out) in decoded.iter().enumerate() {
			let (luma, _, _) = plane_averages(out);
			let want = expected_luma(level(i as u64));
			// Half the gap between adjacent levels, so a frame showing a neighbour's
			// picture fails rather than squeaking through.
			assert!(
				luma.abs_diff(want) <= 6,
				"frame {i} decoded to luma {luma}, expected about {want}: the decoder recycled its picture buffer"
			);
		}
	}

	/// A height that isn't a whole number of macroblocks is coded padded (180 rows
	/// become 192), and the frame has to be the picture rather than the padding.
	///
	/// Chroma is the assertion that bites: the interleaved UV plane starts after
	/// the *texture's* luma rows, so reading a padded texture as if it were the
	/// frame lands in the last luma rows and colors the picture with them.
	#[cfg(all(target_os = "windows", feature = "openh264"))]
	#[test]
	fn mediafoundation_decode_crops_coded_padding() {
		let size = crate::Size::new(320, 180);
		let Some((decoded, _decoder)) = decode_levels(3, size) else {
			return;
		};

		for (i, out) in decoded.iter().enumerate() {
			assert_eq!(out.size(), size, "frame {i} came back at the coded size");
			let (luma, u, v) = plane_averages(out);
			assert!(
				luma.abs_diff(expected_luma(level(i as u64))) <= 6,
				"frame {i} luma {luma} is not its own picture"
			);
			// Gray in, so both chroma planes stay neutral.
			assert!(
				u.abs_diff(128) <= 4 && v.abs_diff(128) <= 4,
				"frame {i} chroma ({u}, {v}) is not neutral: the plane split read into the padding"
			);
		}
	}

	/// The transcode path stays on hardware from decode through re-encode: the
	/// hardware encoder MFT takes the decoded texture on the same Direct3D11
	/// device, no download and no upload. The residency assertion catches a CPU
	/// fallback even when the pixels and dimensions still look right.
	///
	/// Decoding what comes back is the other half, and the one that pins the blit:
	/// the encoder reads the texture on its own timeline, so a copy that never
	/// landed still produces packets, just of the wrong picture.
	#[cfg(all(target_os = "windows", feature = "openh264"))]
	#[test]
	fn mediafoundation_decoded_texture_reencodes_in_place() {
		let size = gray_size();
		let Some((decoded, _decoder)) = decode_levels(3, size) else {
			return;
		};
		for out in &decoded {
			assert!(
				matches!(out.surface, Surface::Texture(_)),
				"Media Foundation decode downloaded to the CPU"
			);
		}

		let encoder = Encoder::new(&EncodeConfig {
			kind: EncodeKind::Named("mediafoundation".into()),
			..EncodeConfig::new(size.width, size.height, crate::Rate::new(30, 1).unwrap())
		});
		let Ok(mut encoder) = encoder else {
			eprintln!("skipping: no Media Foundation H.264 hardware encoder available");
			return;
		};

		let mut reencoded = Vec::new();
		for (i, out) in decoded.iter().enumerate() {
			if i == 0 {
				encoder.cut().unwrap();
			}
			reencoded.extend(encoder.encode(out).unwrap());
		}
		reencoded.extend(encoder.finish().unwrap());
		assert!(
			!reencoded.is_empty(),
			"re-encoding decoded textures produced no packets"
		);

		// Back to pixels through the software decoder, so this leans on nothing the
		// hardware path just did.
		let mut decoder = backend::open(Codec::H264, &decode_config(super::Kind::Software)).expect("openh264 decoder");
		let mut out = Vec::new();
		for (i, encoded) in reencoded.iter().enumerate() {
			out.extend(
				decoder
					.decode(encoded.payload.clone(), encoded.timestamp, i == 0)
					.unwrap(),
			);
		}

		// Every frame, not merely some: a hardware encoder holding its tail back is
		// what this file's flush exists to stop, and a per-frame check alone cannot
		// see a stream that came back one short.
		assert_eq!(out.len(), decoded.len(), "the re-encoded stream lost frames");
		for (i, frame) in out.iter().enumerate() {
			assert_eq!(frame.size(), size, "re-encoded frame {i} changed size");
			let (luma, _, _) = plane_averages(frame);
			let want = expected_luma(level(i as u64));
			assert!(
				luma.abs_diff(want) <= 6,
				"re-encoded frame {i} came back as luma {luma}, expected about {want}"
			);
		}
	}

	/// The multi-rung transcode path stays on hardware through decode, resize, and
	/// encode: the Direct3D11 video processor scales the decoded texture on its own
	/// device and the encoder MFT reads the result in place. The residency
	/// assertion catches a CPU fallback even when the pixels and dimensions still
	/// look right, which is what a ladder pays for once per rung.
	#[cfg(all(target_os = "windows", feature = "openh264"))]
	#[test]
	#[ignore = "explicit live-DXVA GPU probe; VideoProcessorBlt can hang on affected drivers"]
	fn mediafoundation_resized_texture_reencodes_in_place() {
		let target = crate::Size::new(160, 120);
		let resize = crate::resize::Config::default();
		let Some((decoded, _decoder)) = decode_levels(3, gray_size()) else {
			return;
		};
		let Some(device) = decoded.iter().find_map(|frame| match &frame.surface {
			Surface::Texture(texture) => Some(texture.device()),
			_ => None,
		}) else {
			panic!("Media Foundation decode did not return a Direct3D11 texture");
		};
		if !crate::frame::d3d11::supports_nv12_render_target(device) {
			eprintln!("skipping: driver cannot render to NV12");
			return;
		}
		let resized: Vec<_> = decoded
			.iter()
			.map(|frame| frame.resize(target, &resize).unwrap())
			.collect();
		for frame in &resized {
			assert_eq!(frame.size(), target);
			assert!(
				matches!(frame.surface, Surface::Texture(_)),
				"Direct3D11 resize downloaded to the CPU"
			);
		}

		let encoder = Encoder::new(&EncodeConfig {
			kind: EncodeKind::Named("mediafoundation".into()),
			..EncodeConfig::new(target.width, target.height, crate::Rate::new(30, 1).unwrap())
		});
		let Ok(mut encoder) = encoder else {
			eprintln!("skipping: no Media Foundation H.264 hardware encoder available");
			return;
		};

		let mut packets = 0;
		for (i, out) in resized.iter().enumerate() {
			if i == 0 {
				encoder.cut().unwrap();
			}
			packets += encoder.encode(out).unwrap().len();
		}
		packets += encoder.finish().unwrap().len();

		assert!(packets > 0, "re-encoding resized textures produced no packets");
	}

	/// H.265 has no software encoder or decoder, so the HEVC round-trip rides the
	/// Media Foundation hardware path on both ends: NVENC/QSV/AMF encode through an
	/// HEVC encoder MFT, DXVA decode through an HEVC decoder MFT. Skips cleanly when
	/// either is absent (no GPU, or no HEVC Video Extensions installed).
	#[cfg(target_os = "windows")]
	#[test]
	fn mediafoundation_hevc_round_trip() {
		let encoder = Encoder::new(&EncodeConfig {
			kind: EncodeKind::Named("mediafoundation".into()),
			codec: crate::encode::Codec::H265,
			..EncodeConfig::new(320, 240, crate::Rate::new(30, 1).unwrap())
		});
		let Ok(encoder) = encoder else {
			eprintln!("skipping: no Media Foundation H.265 hardware encoder available");
			return;
		};
		let Ok(decoder) = backend::open(
			Codec::H265,
			&decode_config(super::Kind::Named("mediafoundation".into())),
		) else {
			eprintln!("skipping: no Media Foundation H.265 hardware decoder available");
			return;
		};
		round_trip(encoder, decoder, "mediafoundation");
	}
}
