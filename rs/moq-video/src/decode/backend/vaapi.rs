//! Intel/AMD VAAPI hardware H.264 decode via the opt-in `moq-vaapi` crate on Linux.
//!
//! The decode half of [`encode::backend::vaapi`](crate::encode). `moq-vaapi` is a
//! focused VA-API H.264 codec vendored and trimmed from cros-libva +
//! discord/cros-codecs. Its decoder takes one Annex-B access unit with the
//! parameter sets inline and hands back tightly-packed NV12, which this
//! deinterleaves to the CPU I420 the rest of the crate speaks.
//!
//! libva is `dlopen`'d at runtime, so a VAAPI-enabled build needs no libva at
//! build time and the binary carries no `NEEDED libva`. A libva-less host, a
//! missing render node, or a driver with no H.264 decode entrypoint makes
//! `Decoder::new` return an error; under automatic selection
//! [`backend::open`](super::open) then moves on to the next candidate, like the
//! NVDEC backend.
//!
//! Progressive 8-bit 4:2:0 only, which is everything a browser's `VideoEncoder`,
//! WebRTC, or this crate's own encoders emit. The decoder rejects an interlaced or
//! high-bit-depth sequence at its first SPS, which surfaces here as a decode
//! error rather than wrong pixels.
//!
//! Unlike NVDEC, which cuvid lets us pin to zero display delay, output trails the
//! input even without B-frames: H.264's DPB releases a picture only once a later
//! one needs its slot, and the slot count comes from the sequence's reference and
//! reorder limits rather than the reorder depth actually used. A stream coded with
//! three reference frames therefore keeps three pictures in hand, which is why
//! this backend implements [`Backend::flush`] and the layers above call it when
//! the track ends. The decoded frames carry their own timestamps, so the delay
//! itself needs nothing downstream.
//!
//! The output is CPU I420 rather than a zero-copy [`Surface::DmaBuf`], which is
//! now a question of plumbing rather than of capability. It used to be the
//! latter: a decode target was believed to be a modifier the renderer could not
//! import. Measured on Intel Meteor Lake with iHD, a decode target exports at
//! `0x100000000000009`, and the renderer imports that per memory plane with no
//! re-tile, so the pixels do reach a texture untouched
//! (`decoded_frames_reach_the_gpu_without_a_download` in
//! [`render`](crate::render) draws them).
//!
//! What stands in the way of making it the default is
//! [`Surface::into_i420`](crate::Surface::into_i420): a CPU consumer of an
//! exported surface has nowhere to read it back from, since the frame outlives
//! the decoder that could map it, and the buffer is tiled so mapping it as rows
//! would be wrong anyway. Handing out GPU frames therefore has to be something a
//! caller asks for, knowing what it will do with them.

use bytes::Bytes;
use moq_net::Timestamp;
use moq_vaapi::decode::{Config as VaapiConfig, Decoder};

use super::{Backend, Codec, Config};
use crate::frame::{I420, Surface};
use crate::{Error, Frame};

pub(crate) const NAME: &str = "vaapi";

pub(crate) struct Vaapi {
	decoder: Decoder,
}

// The decoder is `!Send` (libva uses `Rc` internally) but is created, used, and
// dropped only on the dedicated decode thread (see `decode::sink`); the `Send`
// impl just lets the boxed trait object satisfy `Backend: Send`.
unsafe impl Send for Vaapi {}

impl Vaapi {
	/// VA-API H.265 and AV1 decode exist but are not wired up in `moq-vaapi`, so
	/// this handles H.264 only. `config` carries no hardware scaler request we can
	/// honor: VA-API's scaler is a separate VPP pipeline, so callers scale the
	/// frames themselves.
	pub(crate) fn open(codec: Codec, _config: &Config) -> Result<Box<dyn Backend>, Error> {
		if codec != Codec::H264 {
			return Err(Error::Codec(anyhow::anyhow!("VAAPI cannot decode {}", codec.label())));
		}

		let decoder =
			Decoder::new(VaapiConfig::new()).map_err(|e| Error::Codec(anyhow::anyhow!("VAAPI decoder init: {e:?}")))?;

		tracing::info!(decoder = NAME, "opened H.264 decoder");
		Ok(Box::new(Self { decoder }))
	}
}

impl Backend for Vaapi {
	fn decode(&mut self, access_unit: Bytes, timestamp: Timestamp, _keyframe: bool) -> Result<Vec<Frame>, Error> {
		// The timestamp rides through the decoder with the picture, so it
		// survives the DPB reordering a stream with B-frames goes through.
		let decoded = self
			.decoder
			.decode(&access_unit, timestamp.as_micros() as u64)
			.map_err(|e| Error::Codec(anyhow::anyhow!("VAAPI decode: {e:?}")))?;

		convert(decoded)
	}

	/// The tail of the stream, which is why this backend overrides the trait's
	/// no-op: the DPB is holding as many pictures as the sequence's reference and
	/// reorder limits allow, and nothing else will ever ask for them.
	fn flush(&mut self) -> Result<Vec<Frame>, Error> {
		let decoded = self
			.decoder
			.flush()
			.map_err(|e| Error::Codec(anyhow::anyhow!("VAAPI flush: {e:?}")))?;

		convert(decoded)
	}

	fn name(&self) -> &str {
		NAME
	}
}

/// Deinterleave each decoded picture's NV12 into the CPU I420 the rest of the
/// crate speaks, keeping the timestamp the picture was coded with.
fn convert(decoded: Vec<moq_vaapi::decode::Frame>) -> Result<Vec<Frame>, Error> {
	decoded
		.into_iter()
		.map(|frame| {
			let i420 = I420::from_nv12(&frame.data, frame.width, frame.height)?;
			let timestamp = Timestamp::from_micros(frame.timestamp).unwrap_or(Timestamp::ZERO);
			Ok(Frame::new(Surface::I420(i420), timestamp))
		})
		.collect()
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::decode::{Config as DecodeConfig, Kind as DecodeKind};
	use crate::encode::{Config as EncodeConfig, Encoder, Kind as EncodeKind};

	/// Real hardware only: skip on a box with no libva or no VA-API H.264 decode
	/// entrypoint, so these are no-ops on CI and validate on an Intel or AMD box.
	fn hw_available() -> bool {
		Decoder::new(VaapiConfig::new()).is_ok()
	}

	fn decode_config() -> DecodeConfig {
		DecodeConfig {
			kind: DecodeKind::Named(NAME.into()),
			..DecodeConfig::new()
		}
	}

	/// On a host with no usable VA stack, opening must return an `Err` (so
	/// `Kind::Auto` falls through to openh264) rather than panicking inside libva.
	/// A no-op on a box that does have one.
	#[test]
	fn missing_driver_errors_instead_of_panicking() {
		if hw_available() {
			return;
		}
		assert!(Vaapi::open(Codec::H264, &decode_config()).is_err());
	}

	/// A static RGBA gradient that varies in both axes, so the chroma planes have
	/// spatial structure and a pitch or plane-split bug corrupts the picture.
	fn gradient_rgba(width: u32, height: u32) -> Vec<u8> {
		let (w, h) = (width as usize, height as usize);
		let mut buf = vec![0u8; w * h * 4];
		for y in 0..h {
			for x in 0..w {
				let i = (y * w + x) * 4;
				buf[i] = (x * 255 / w) as u8;
				buf[i + 1] = (y * 255 / h) as u8;
				buf[i + 2] = ((x + y) * 255 / (w + h)) as u8;
				buf[i + 3] = 255;
			}
		}
		buf
	}

	/// Mean absolute error between two equal-length planes.
	fn mae(a: &[u8], b: &[u8]) -> u64 {
		assert_eq!(a.len(), b.len());
		a.iter().zip(b).map(|(x, y)| x.abs_diff(*y) as u64).sum::<u64>() / a.len() as u64
	}

	/// H.264 through the real hardware: openh264 encodes a gradient (Annex-B with
	/// inline SPS/PPS) and VA-API decodes it. Asserts the downloaded picture
	/// matches the input, which a plane split or stride bug would shear, and that
	/// the timestamp rides through with its picture.
	#[test]
	fn vaapi_h264_round_trip() {
		if !hw_available() {
			return;
		}
		let (w, h) = (320u32, 240u32);
		let rgba = gradient_rgba(w, h);
		let expected = I420::from_rgba(&rgba, w * 4, w, h).unwrap();

		let mut encoder = Encoder::new(&EncodeConfig {
			kind: EncodeKind::Software,
			..EncodeConfig::new(w, h, 30)
		})
		.unwrap();
		let mut decoder = Vaapi::open(Codec::H264, &decode_config()).expect("VAAPI H.264 decoder");

		let mut decoded = Vec::new();
		for i in 0..10u64 {
			if i == 0 {
				encoder.keyframe();
			}
			let surface = Surface::rgba(&rgba, crate::Size::new(w, h)).unwrap();
			let frame = Frame::new(surface, Timestamp::from_micros(i * 33_333).unwrap());
			for encoded in encoder.encode(&frame).unwrap() {
				decoded.extend(decoder.decode(encoded.payload, encoded.timestamp, i == 0).unwrap());
			}
		}

		assert!(!decoded.is_empty(), "VAAPI produced no frames");
		for (i, frame) in decoded.iter().enumerate() {
			// openh264 encodes IPPP, so the pictures come back in feed order.
			assert_eq!(
				frame.timestamp.as_micros(),
				i as u128 * 33_333,
				"timestamp did not ride the picture"
			);
			let i420 = frame.surface.to_i420().unwrap();
			assert_eq!((i420.width(), i420.height()), (w, h));
			assert!(mae(i420.y(), expected.y()) < 8, "Y plane corrupt");
			assert!(mae(i420.u(), expected.u()) < 8, "U plane corrupt");
			assert!(mae(i420.v(), expected.v()) < 8, "V plane corrupt");
		}
	}

	/// Regression: every picture fed in comes back out, which needs the flush.
	///
	/// H.264 releases a picture from the DPB only once a later one needs its slot,
	/// so a stream simply stopping leaves its tail there. How many depends on the
	/// sequence's reference and reorder limits, not on the reorder depth the
	/// stream used: openh264 codes one reference frame and loses the last picture,
	/// x264's default three loses three. The `decode` half of the assertion is
	/// what keeps this honest, since a decoder that held nothing back would pass
	/// the rest of it without a flush ever running.
	#[test]
	fn flushing_returns_the_tail_the_dpb_holds() {
		if !hw_available() {
			return;
		}
		const FRAMES: u64 = 5;
		let (w, h) = (320u32, 240u32);
		let rgba = gradient_rgba(w, h);

		let mut encoder = Encoder::new(&EncodeConfig {
			kind: EncodeKind::Software,
			..EncodeConfig::new(w, h, 30)
		})
		.unwrap();
		let mut decoder = Vaapi::open(Codec::H264, &decode_config()).expect("VAAPI H.264 decoder");

		let mut streamed = Vec::new();
		for i in 0..FRAMES {
			if i == 0 {
				encoder.keyframe();
			}
			let surface = Surface::rgba(&rgba, crate::Size::new(w, h)).unwrap();
			let frame = Frame::new(surface, Timestamp::from_micros(i * 33_333).unwrap());
			for encoded in encoder.encode(&frame).unwrap() {
				streamed.extend(decoder.decode(encoded.payload, encoded.timestamp, i == 0).unwrap());
			}
		}
		assert!(
			(streamed.len() as u64) < FRAMES,
			"the DPB held nothing back, so this test proves nothing"
		);

		let flushed = decoder.flush().unwrap();
		let timestamps: Vec<u128> = streamed
			.iter()
			.chain(&flushed)
			.map(|frame| frame.timestamp.as_micros())
			.collect();
		let expected: Vec<u128> = (0..FRAMES as u128).map(|i| i * 33_333).collect();
		assert_eq!(timestamps, expected, "the stream lost pictures at its end");

		// A second flush has nothing left to hand back, so the drain is idempotent
		// and a caller that flushes twice does not see a picture twice.
		assert!(decoder.flush().unwrap().is_empty());
	}
}
