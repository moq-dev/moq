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
//! The output is CPU I420 unless [`Config::gpu_frames`] asks otherwise, in which
//! case each picture comes back as the zero-copy [`Surface::DmaBuf`] the
//! hardware decoded it into. Measured on Intel Meteor Lake with iHD, a decode
//! target exports at modifier `0x100000000000009`, and the renderer imports that
//! a memory plane at a time, so the pixels reach a texture untouched
//! (`decoded_frames_reach_the_gpu_without_a_download` in
//! [`render`](crate::render) draws them).
//!
//! Opt-in rather than the default because it is not free to a consumer that does
//! not draw. Exporting a surface retires it from the decoder's recycling pool,
//! since a later picture decoded over it would corrupt one the consumer still
//! holds, so it trades an allocation per picture for a download that a CPU
//! consumer would have to pay anyway. Such a consumer is not stranded either:
//! [`Surface::into_i420`](crate::Surface::into_i420) still answers, because
//! `moq-vaapi` keeps the retired surface alongside the descriptor and reads it
//! back through `vaDeriveImage` rather than trying to read a tiled buffer as
//! rows.

use std::os::fd::{AsFd, OwnedFd};
use std::sync::{Arc, Mutex};

use bytes::Bytes;
use moq_net::Timestamp;
use moq_vaapi::decode::{Config as VaapiConfig, Decoder, ExportedFrame};

use super::{Backend, Codec, Config};
use crate::frame::{DmaBuf, DmaBufFrame, DmaBufPlane, DrmFormat, I420, Surface};
use crate::{Error, Frame};

pub(crate) const NAME: &str = "vaapi";

pub(crate) struct Vaapi {
	decoder: Decoder,
	/// Whether pictures are handed out as DMA-BUFs rather than downloaded, from
	/// [`Config::gpu_frames`]. Cleared if the driver turns out not to export, so
	/// a host that decodes but cannot share what it decoded loses the fast path
	/// rather than the stream.
	gpu_frames: bool,
	/// Whether a picture has ever come back as a descriptor, which is what
	/// settles the question above. See [`Vaapi::exported`].
	has_exported: bool,
}

// SAFETY: the decoder is `!Send` (libva uses `Rc` internally) but is created,
// used, and dropped only on the dedicated decode thread (see `decode::sink`);
// the `Send` impl just lets the boxed trait object satisfy `Backend: Send`.
// None of the `Rc`s escape with a picture either: an exported one holds its own
// `Arc<Display>` and a surface id, and is `Send` and `Sync` on its own terms.
unsafe impl Send for Vaapi {}

impl Vaapi {
	/// VA-API H.265 and AV1 decode exist but are not wired up in `moq-vaapi`, so
	/// this handles H.264 only. `config` carries no hardware scaler request we can
	/// honor: VA-API's scaler is a separate VPP pipeline, so callers scale the
	/// frames themselves.
	pub(crate) fn open(codec: Codec, config: &Config) -> Result<Box<dyn Backend>, Error> {
		if codec != Codec::H264 {
			return Err(Error::Codec(anyhow::anyhow!("VAAPI cannot decode {}", codec.label())));
		}

		let decoder =
			Decoder::new(VaapiConfig::new()).map_err(|e| Error::Codec(anyhow::anyhow!("VAAPI decoder init: {e:?}")))?;

		tracing::info!(decoder = NAME, gpu_frames = config.gpu_frames, "opened H.264 decoder");
		Ok(Box::new(Self {
			decoder,
			gpu_frames: config.gpu_frames,
			has_exported: false,
		}))
	}

	/// Decodes one access unit into GPU-resident frames, or `None` once the
	/// driver has shown it will not export.
	fn decode_shared(&mut self, access_unit: &Bytes, timestamp: u64) -> Option<Result<Vec<Frame>, Error>> {
		if !self.gpu_frames {
			return None;
		}

		let exported = self.decoder.decode_exported(access_unit, timestamp).and_then(share);
		Some(self.exported(exported))
	}

	/// The same for the stream's tail, so a track that ends does not switch to
	/// downloading for its last few pictures.
	fn flush_shared(&mut self) -> Option<Result<Vec<Frame>, Error>> {
		if !self.gpu_frames {
			return None;
		}

		let exported = self.decoder.flush_exported().and_then(share);
		Some(self.exported(exported))
	}

	/// Hands back the pictures a shared decode produced, and decides what a
	/// failure to produce any means.
	///
	/// A driver can decode without being able to share what it decoded, and the
	/// caller only asked for GPU frames as an optimization, so until one picture
	/// has come back that way a failure is read as this driver answering the
	/// question. Losing the pictures of the call that found out beats losing the
	/// stream, and the DPB is untouched by it: those pictures had already been
	/// bumped out of it.
	///
	/// Once a picture has come back the question is settled, and a later failure
	/// is a decode error rather than a verdict on the driver. Reporting it as
	/// one keeps a corrupt access unit from silently costing the rest of the
	/// session its fast path.
	fn exported(&mut self, exported: anyhow::Result<Vec<Frame>>) -> Result<Vec<Frame>, Error> {
		match exported {
			Ok(frames) => {
				// An access unit whose pictures are all still in the DPB proves
				// nothing about exporting, so only a non-empty answer counts.
				self.has_exported |= !frames.is_empty();
				Ok(frames)
			}
			Err(err) if self.has_exported => Err(Error::Codec(err.context("VAAPI decode to a shared surface"))),
			Err(err) => {
				tracing::warn!(%err, "VAAPI cannot hand out decoded surfaces; downloading them instead");
				self.gpu_frames = false;
				Ok(Vec::new())
			}
		}
	}
}

impl Backend for Vaapi {
	fn decode(&mut self, access_unit: Bytes, timestamp: Timestamp, _keyframe: bool) -> Result<Vec<Frame>, Error> {
		// The timestamp rides through the decoder with the picture, so it
		// survives the DPB reordering a stream with B-frames goes through.
		let timestamp = timestamp.as_micros() as u64;
		if let Some(frames) = self.decode_shared(&access_unit, timestamp) {
			return frames;
		}

		let decoded = self
			.decoder
			.decode(&access_unit, timestamp)
			.map_err(|e| Error::Codec(anyhow::anyhow!("VAAPI decode: {e:?}")))?;

		convert(decoded)
	}

	/// The tail of the stream, which is why this backend overrides the trait's
	/// no-op: the DPB is holding as many pictures as the sequence's reference and
	/// reorder limits allow, and nothing else will ever ask for them.
	fn flush(&mut self) -> Result<Vec<Frame>, Error> {
		if let Some(frames) = self.flush_shared() {
			return frames;
		}

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

/// Wrap each exported picture as the DMA-BUF surface a renderer imports, keeping
/// the timestamp the picture was coded with.
fn share(exported: Vec<ExportedFrame>) -> anyhow::Result<Vec<Frame>> {
	exported
		.into_iter()
		.map(|frame| {
			let timestamp = Timestamp::from_micros(frame.timestamp).unwrap_or(Timestamp::ZERO);
			Ok(Frame::new(Surface::DmaBuf(adopt(frame)?), timestamp))
		})
		.collect()
}

/// Describe an exported picture as a [`DmaBuf`]: the driver's format modifier,
/// and the offset and pitch of each of its memory planes.
///
/// The width and height are the visible frame rather than the exported extent,
/// which is the driver's padded allocation. Neither the pitches nor the offsets
/// follow from the visible size, which is exactly why they are read off the
/// export rather than computed from it.
///
/// # Errors
///
/// When the export is not the one shape a [`DmaBuf`] can describe: a single NV12
/// layer whose planes all live in a single object. The Intel and AMD drivers
/// export exactly that, and the alternatives are refused rather than guessed at,
/// because every one of them draws as a plausible-looking picture made of the
/// wrong bytes.
fn adopt(frame: ExportedFrame) -> anyhow::Result<DmaBuf> {
	let (width, height) = (frame.width, frame.height);
	// One object, because a consumer imports every plane from the one descriptor
	// `Exported::export` hands out, and one layer, because the planes are read
	// off it as a group. Both are what `VA_EXPORT_SURFACE_COMPOSED_LAYERS` asks
	// for; neither is what it guarantees.
	let [object] = frame.descriptor.objects.as_slice() else {
		anyhow::bail!(
			"VA-API exported {} objects, expected one holding every plane",
			frame.descriptor.objects.len()
		);
	};
	let [layer] = frame.descriptor.layers.as_slice() else {
		anyhow::bail!(
			"VA-API exported {} layers, expected one composed layer",
			frame.descriptor.layers.len()
		);
	};
	if layer.drm_format != DrmFormat::NV12.as_raw() {
		anyhow::bail!("VA-API exported DRM format {:#x}, expected NV12", layer.drm_format);
	}

	// `num_planes` and the arrays it indexes both come from the driver, and only
	// the arrays are bounded, so indexing on the count would panic rather than
	// fail.
	let count = layer.num_planes as usize;
	anyhow::ensure!(
		count <= layer.offset.len(),
		"VA-API exported {count} planes, more than a PRIME descriptor holds"
	);
	let planes = (0..count)
		.map(|plane| DmaBufPlane::new(layer.offset[plane], layer.pitch[plane]))
		.collect();
	let modifier = object.drm_format_modifier;

	// A decode target names no color space, so the renderer infers one from the
	// frame size exactly as it does for a downloaded picture.
	DmaBuf::new(
		DrmFormat::NV12,
		modifier,
		width,
		height,
		planes,
		None,
		Arc::new(Exported::new(frame)),
	)
	.map_err(|e| anyhow::anyhow!("{e}"))
}

/// A decoded picture the consumer holds as a DMA-BUF.
///
/// Both of the things a consumer can do with one: hand a descriptor to a
/// graphics API, or give up on drawing it and read the pixels back. Dropping the
/// last clone destroys the surface, which is what returns its allocation to the
/// driver.
struct Exported {
	/// Locked because [`DmaBufFrame`] hands out `&self` while what is behind it
	/// is a single libva surface: `download_i420` maps that surface, and two
	/// threads doing so at once is more than libva promises to serialize. The
	/// frame is [`Send`] on its own, so a lock is enough and no `unsafe impl` is
	/// involved.
	frame: Mutex<ExportedFrame>,
}

impl Exported {
	fn new(frame: ExportedFrame) -> Self {
		Self {
			frame: Mutex::new(frame),
		}
	}
}

impl DmaBufFrame for Exported {
	/// Vulkan takes ownership of an imported descriptor on success and closes it
	/// on failure, so every import needs one of its own and the original stays
	/// with the picture.
	fn export(&self) -> std::io::Result<OwnedFd> {
		let frame = self.frame.lock().expect("poisoned");
		let object = frame.descriptor.objects.first().ok_or_else(|| {
			std::io::Error::new(std::io::ErrorKind::InvalidData, "the VA-API export carries no object")
		})?;
		object.fd.as_fd().try_clone_to_owned()
	}

	/// Read the picture back through the retained surface rather than the
	/// descriptor: a decode target is tiled, so mapping the file descriptor as
	/// rows would be wrong.
	fn download_i420(&self) -> Result<I420, Error> {
		let frame = self.frame.lock().expect("poisoned");
		let nv12 = frame
			.download()
			.map_err(|e| Error::Codec(anyhow::anyhow!("read a VA-API decode surface back: {e:?}")))?;
		I420::from_nv12(&nv12.data, nv12.width, nv12.height)
	}
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

	fn gpu_decode_config() -> DecodeConfig {
		DecodeConfig {
			gpu_frames: true,
			..decode_config()
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

	/// `gpu_frames` hands out DMA-BUFs, and a CPU consumer of one gets the same
	/// pixels it would have got without asking for them.
	///
	/// The bargain the knob rests on: a caller opting into GPU-resident output
	/// does not take `Surface::into_i420` away from whatever it hands the frames
	/// to. Byte-exact rather than approximate, because both sides are the same
	/// hardware decoding the same units, and the read-back goes through the same
	/// `vaDeriveImage` path the download does.
	///
	/// Needs no GPU beyond the VA-API device: this is about what a picture on the
	/// GPU can still do for a consumer that is not on one.
	#[test]
	fn gpu_frames_still_answer_into_i420() {
		if !hw_available() {
			return;
		}
		let (w, h) = (320u32, 240u32);
		let rgba = gradient_rgba(w, h);

		let mut encoder = Encoder::new(&EncodeConfig {
			kind: EncodeKind::Software,
			..EncodeConfig::new(w, h, 30)
		})
		.unwrap();
		let mut exporting = Vaapi::open(Codec::H264, &gpu_decode_config()).expect("VAAPI H.264 decoder");
		let mut downloading = Vaapi::open(Codec::H264, &decode_config()).expect("a second decoder");

		let mut exported = Vec::new();
		let mut downloaded = Vec::new();
		for i in 0..10u64 {
			if i == 0 {
				encoder.keyframe();
			}
			let surface = Surface::rgba(&rgba, crate::Size::new(w, h)).unwrap();
			let frame = Frame::new(surface, Timestamp::from_micros(i * 33_333).unwrap());
			for encoded in encoder.encode(&frame).unwrap() {
				let (payload, timestamp) = (encoded.payload, encoded.timestamp);
				exported.extend(exporting.decode(payload.clone(), timestamp, i == 0).unwrap());
				downloaded.extend(downloading.decode(payload, timestamp, i == 0).unwrap());
			}
		}
		assert!(!exported.is_empty(), "VAAPI produced no frames");
		assert_eq!(exported.len(), downloaded.len(), "the two decoders disagreed");

		for (i, (gpu, cpu)) in exported.iter().zip(&downloaded).enumerate() {
			let Surface::DmaBuf(buffer) = &gpu.surface else {
				panic!("frame {i} did not come back GPU-resident");
			};
			assert_eq!(buffer.format(), crate::DrmFormat::NV12);
			assert_eq!((buffer.width(), buffer.height()), (w, h));
			assert_eq!(gpu.timestamp, cpu.timestamp, "frame {i} lost its timestamp");

			let Surface::I420(reference) = &cpu.surface else {
				panic!("frame {i} came back GPU-resident without gpu_frames");
			};
			let read_back = gpu.surface.to_i420().expect("read the decoded surface back");
			assert_eq!(
				(read_back.width(), read_back.height()),
				(reference.width(), reference.height())
			);
			assert!(
				read_back.y() == reference.y(),
				"frame {i} read back a different Y plane"
			);
			assert!(
				read_back.u() == reference.u(),
				"frame {i} read back a different U plane"
			);
			assert!(
				read_back.v() == reference.v(),
				"frame {i} read back a different V plane"
			);
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
		flushing_returns_the_tail(&decode_config());
	}

	/// The same for GPU-resident output, which drains through `flush_exported`
	/// rather than `flush`. Without its own case it would be the one path where a
	/// track's last pictures go missing, since the two drains are separate calls
	/// into `moq-vaapi` and only one of them is on the default path.
	#[test]
	fn flushing_returns_the_tail_of_a_gpu_stream() {
		if !hw_available() {
			return;
		}
		flushing_returns_the_tail(&gpu_decode_config());
	}

	fn flushing_returns_the_tail(config: &DecodeConfig) {
		const FRAMES: u64 = 5;
		let (w, h) = (320u32, 240u32);
		let rgba = gradient_rgba(w, h);

		let mut encoder = Encoder::new(&EncodeConfig {
			kind: EncodeKind::Software,
			..EncodeConfig::new(w, h, 30)
		})
		.unwrap();
		let mut decoder = Vaapi::open(Codec::H264, config).expect("VAAPI H.264 decoder");

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
