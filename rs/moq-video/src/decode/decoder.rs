//! Video decoder front end.
//!
//! Prepares each container frame for a [`Backend`](super::backend::Backend):
//! converts out-of-band payloads (avc1 / hvc1: length-prefixed NALs with the
//! parameter sets in the description) to Annex-B and injects those parameter sets
//! ahead of keyframes, leaving in-band H.264 / H.265 payloads (avc3 / hev1,
//! already Annex-B inline) and AV1 OBU temporal units untouched. Gates output
//! until the first keyframe so the backend never sees a delta frame it can't
//! decode.

use std::time::Duration;

use bytes::Bytes;
use hang::catalog::{AV1, VideoCodec, VideoConfig};
use moq_mux::codec::{annexb, h264, h265};
use moq_net::Timestamp;

use super::backend::{self, Backend, Codec};
use crate::{Error, Frame, Size};

/// Which decoder implementation to use. `#[non_exhaustive]` so new selection
/// strategies can be added without breaking external `match`es.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub enum Kind {
	/// Prefer a platform hardware decoder, fall back to software.
	#[default]
	Auto,
	/// Hardware only; error if none is available.
	Hardware,
	/// Software (openh264) only.
	Software,
	/// A specific backend by name, e.g. `"videotoolbox"`, `"nvdec"`,
	/// `"openh264"`.
	Named(String),
}

/// Decoder configuration.
///
/// `#[non_exhaustive]`: build via [`Config::new`] (or `default()`) and set the
/// optional fields, so future knobs don't break callers.
#[derive(Clone, Debug, Default)]
#[non_exhaustive]
pub struct Config {
	/// Which backend to use.
	pub kind: Kind,
	/// Upper bound on buffering before a stalled group is skipped. `None` uses
	/// the moq-mux default (skip aggressively); set it to your playout buffer for
	/// a softer skip. Forwarded to the container consumer's `with_latency`.
	pub latency_max: Option<Duration>,
	/// Ask the decoder to emit frames at this size (both dimensions even) instead
	/// of the stream's native one. Best effort: a hardware decoder with a
	/// built-in scaler (NVDEC) honors it for free, other backends ignore it.
	/// Check each [`Frame`](crate::Frame)'s dimensions and scale the remainder
	/// yourself.
	pub resize: Option<Size>,
}

impl Config {
	/// A default config: automatic backend selection, default latency.
	pub fn new() -> Self {
		Self::default()
	}
}

/// How to turn a container payload into a backend access unit.
enum Conversion {
	/// The payload is already in the backend's input framing: Annex-B for avc3 /
	/// hev1, OBU temporal units for AV1.
	Passthrough,
	/// avc1 / hvc1: length-prefixed NALs with the parameter sets out-of-band (in
	/// the avcC / hvcC description). Replace the length prefixes with start codes
	/// and prepend `keyframe_prefix` (the parameter sets) ahead of every keyframe.
	LengthPrefixed { length_size: usize, keyframe_prefix: Bytes },
}

/// Decodes container payloads (the codec bitstream) into raw [`Frame`]s.
///
/// The bring-your-own-payload layer under [`Consumer`](super::Consumer): use it
/// when the frames don't come from a plain track subscription, e.g. a transcoder
/// serving individually fetched groups. Feed it the payload of each container
/// frame in decode order; it handles avc1/hvc1 -> Annex-B conversion, passes
/// AV1 OBU temporal units through, and gates output until the first keyframe.
pub struct Decoder {
	backend: Box<dyn Backend>,
	conversion: Conversion,
	got_keyframe: bool,
}

impl Decoder {
	/// Build a decoder for the catalog's video config. Errors if the codec is
	/// not supported by the native backends.
	pub fn new(catalog: &VideoConfig, config: &Config) -> Result<Self, Error> {
		let (codec, conversion) = match &catalog.codec {
			VideoCodec::H264(h264) => {
				let conversion = if h264.inline {
					Conversion::Passthrough
				} else {
					let avcc = catalog.description.as_ref().ok_or_else(|| {
						Error::Codec(anyhow::anyhow!("avc1 H.264 track is missing its avcC description"))
					})?;
					let params = h264::Avcc::parse(avcc).map_err(moq_mux::Error::from)?;
					let keyframe_prefix = annexb::build_prefix(params.sps.iter().chain(params.pps.iter()));
					Conversion::LengthPrefixed {
						length_size: params.length_size,
						keyframe_prefix,
					}
				};
				(Codec::H264, conversion)
			}
			VideoCodec::H265(h265) => {
				let conversion = if h265.in_band {
					Conversion::Passthrough
				} else {
					let hvcc = catalog.description.as_ref().ok_or_else(|| {
						Error::Codec(anyhow::anyhow!("hvc1 H.265 track is missing its hvcC description"))
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
			other => return Err(Error::UnsupportedCodec(other.to_string())),
		};

		let backend = backend::open(codec, config)?;
		tracing::debug!(decoder = backend.name(), "opened video decoder");
		Ok(Self {
			backend,
			conversion,
			got_keyframe: false,
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

		let access_unit = match &self.conversion {
			// Cheap refcount bump; the backend splits codec units off this buffer.
			Conversion::Passthrough => payload.clone(),
			Conversion::LengthPrefixed {
				length_size,
				keyframe_prefix,
			} => {
				let prefix = keyframe.then(|| keyframe_prefix.as_ref());
				annexb::from_length_prefixed(payload, *length_size, prefix).map_err(moq_mux::Error::from)?
			}
		};

		self.backend.decode(access_unit, timestamp, keyframe)
	}
}

fn is_supported_av1(av1: &AV1) -> bool {
	av1.bitdepth == 8 && !av1.mono_chrome && av1.chroma_subsampling_x && av1.chroma_subsampling_y
}

#[cfg(test)]
mod tests {
	use moq_net::Timestamp;

	use super::backend::{self, Codec};
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
				encoder.keyframe();
			}
			// Distinct, spread-apart timestamps so a round-tripped value is unambiguous.
			for encoded in encoder.encode(&gray_frame(i)).unwrap() {
				decoded.extend(decoder.decode(encoded.payload, encoded.timestamp, keyframe).unwrap());
			}
		}

		assert!(!decoded.is_empty(), "decoder produced no frames");
		for out in &decoded {
			assert_gray(&out.surface.to_i420().unwrap(), 320, 240);
		}

		// The timestamp rides through the codec and comes back on each picture. These
		// backends don't reorder, so it returns in feed order: strictly increasing and
		// drawn from the values we fed.
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

	/// An openh264 (software H.264) encoder for a `size` test stream at 30fps.
	fn h264_software_encoder(size: crate::Size) -> Encoder {
		Encoder::new(&EncodeConfig {
			kind: EncodeKind::Software,
			..EncodeConfig::new(size.width, size.height, 30)
		})
		.expect("openh264 encoder")
	}

	/// The size the gray test stream is encoded at.
	fn gray_size() -> crate::Size {
		crate::Size::new(320, 240)
	}

	#[test]
	fn openh264_round_trip() {
		let decoder = backend::open(Codec::H264, &decode_config(super::Kind::Software)).expect("openh264 decoder");
		round_trip(h264_software_encoder(gray_size()), decoder, "openh264");
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

	#[cfg(target_os = "macos")]
	#[test]
	fn videotoolbox_round_trip() {
		let decoder = backend::open(Codec::H264, &decode_config(super::Kind::Named("videotoolbox".into())))
			.expect("videotoolbox decoder");
		round_trip(h264_software_encoder(gray_size()), decoder, "videotoolbox");
	}

	/// Encode `count` gray frames and decode them, returning the decoded pictures.
	/// The shared setup for the residency and re-encode tests below.
	#[cfg(target_os = "macos")]
	fn decode_gray(count: u64) -> Vec<Frame> {
		let mut encoder = h264_software_encoder(gray_size());
		let mut decoder = backend::open(Codec::H264, &decode_config(super::Kind::Named("videotoolbox".into())))
			.expect("videotoolbox decoder");

		let mut decoded = Vec::new();
		for i in 0..count {
			let keyframe = i == 0;
			if keyframe {
				encoder.keyframe();
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
	#[cfg(target_os = "macos")]
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
	#[cfg(target_os = "macos")]
	#[test]
	fn videotoolbox_resized_surface_reencodes_in_place() {
		let decoded = decode_gray(3);
		let resized: Vec<_> = decoded
			.iter()
			.map(|frame| frame.resize(crate::Size::new(160, 120)).unwrap())
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
			..EncodeConfig::new(160, 120, 30)
		});
		let Ok(mut encoder) = encoder else {
			eprintln!("skipping: no VideoToolbox H.264 hardware encoder available");
			return;
		};

		let mut packets = 0;
		for (i, out) in resized.iter().enumerate() {
			if i == 0 {
				encoder.keyframe();
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
	#[cfg(target_os = "macos")]
	#[test]
	fn videotoolbox_hevc_round_trip() {
		let encoder = Encoder::new(&EncodeConfig {
			kind: EncodeKind::Named("videotoolbox".into()),
			codec: crate::encode::Codec::H265,
			..EncodeConfig::new(320, 240, 30)
		});
		let Ok(encoder) = encoder else {
			eprintln!("skipping: no VideoToolbox H.265 hardware encoder available");
			return;
		};
		let decoder = backend::open(Codec::H265, &decode_config(super::Kind::Named("videotoolbox".into())))
			.expect("videotoolbox H.265 decoder");
		round_trip(encoder, decoder, "videotoolbox");
	}

	#[cfg(target_os = "windows")]
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
	#[cfg(target_os = "windows")]
	fn level(index: u64) -> u8 {
		u8::try_from(0x20 + index * 0x10).expect("test stream is short enough to keep its levels distinct")
	}

	/// The limited-range BT.601 luma a flat [`level`] frame decodes to.
	#[cfg(target_os = "windows")]
	fn expected_luma(level: u8) -> u32 {
		16 + (219 * level as u32) / 255
	}

	/// Decode `count` frames of a `size` [`level`] stream through the Media
	/// Foundation hardware decoder, holding every picture rather than consuming it
	/// as it arrives. `None` when this machine has no hardware decoder.
	#[cfg(target_os = "windows")]
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
				encoder.keyframe();
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
	#[cfg(target_os = "windows")]
	fn plane_averages(frame: &Frame) -> (u32, u32, u32) {
		let i420 = frame.surface.to_i420().unwrap();
		let average = |plane: &[u8]| plane.iter().map(|&b| b as u32).sum::<u32>() / plane.len() as u32;
		(average(i420.y()), average(i420.u()), average(i420.v()))
	}

	/// A decoded frame comes back as a GPU texture rather than downloaded pixels,
	/// which is what leaves a render or re-encode path free of a CPU round trip.
	/// `round_trip` above only checks the pixels, so it passes either way: this is
	/// the test that pins the frame's residency.
	#[cfg(target_os = "windows")]
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
	#[cfg(target_os = "windows")]
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
	#[cfg(target_os = "windows")]
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
	#[cfg(target_os = "windows")]
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
			..EncodeConfig::new(size.width, size.height, 30)
		});
		let Ok(mut encoder) = encoder else {
			eprintln!("skipping: no Media Foundation H.264 hardware encoder available");
			return;
		};

		let mut reencoded = Vec::new();
		for (i, out) in decoded.iter().enumerate() {
			if i == 0 {
				encoder.keyframe();
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
			..EncodeConfig::new(320, 240, 30)
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
