//! Subscribe to an H.264, H.265, or AV1 track and decode it to raw frames.
//!
//! The decode counterpart to [`encode`](crate::encode), and the mirror of
//! `moq_audio::decode::Consumer`. [`Consumer`] subscribes to a moq-mux video
//! track and hands back decoded [`Frame`]s; a native backend does the work
//! (VideoToolbox on macOS, Media Foundation / DXVA on Windows, NVDEC on Linux,
//! openh264 everywhere as the software fallback for H.264).
//!
//! H.264 and H.265 are supported, symmetric with what [`encode`](crate::encode)
//! produces. AV1 is decode-only on NVDEC. H.265 and AV1 are hardware-only (no
//! software fallback). Any other codec yields
//! [`Error::UnsupportedCodec`](crate::Error).

use bytes::Bytes;
use moq_net::Timestamp;

use crate::{Error, Size};

// Crate-visible so the NVENC encode backend's round-trip test can decode its
// output with the software decoder (an in-crate, ffmpeg-free encode->decode
// check that catches input-pitch corruption).
pub(crate) mod backend;
mod consumer;
mod decoder;

pub use consumer::Consumer;
pub use decoder::{Config, Decoder, Kind};

/// A decoded raw video frame: CPU I420, or a GPU frame when a hardware decoder
/// produced one (NVDEC on Linux).
///
/// A GPU frame stays on the GPU until something needs bytes: feeding it to
/// [`encode::Encoder::encode`](crate::encode::Encoder::encode) keeps it there
/// (the zero-copy transcode path), while [`into_i420`](Self::into_i420)
/// downloads it.
pub struct Frame {
	/// Presentation timestamp, carried through from the container. It rides out of
	/// the decoder with each picture, so a reordered frame (B-frames) keeps its own
	/// time rather than the input access unit's.
	pub timestamp: Timestamp,
	/// The decoded resolution, which is [`Config::resize`] when the backend
	/// honored it and the stream's native size otherwise.
	pub size: Size,
	/// The pixels: CPU I420 or a GPU surface.
	pub(crate) inner: crate::frame::Frame,
}

impl Frame {
	/// A copy of this frame scaled to `size` (both dimensions even and non-zero),
	/// preserving the timestamp. A CUDA frame scales on the GPU (a box filter,
	/// correct at any downscale factor) and stays there, so resize ->
	/// [`encode`](crate::encode::Encoder::encode) never touches the CPU. Every
	/// other frame scales on the CPU, which for a `CVPixelBuffer` means a
	/// download first. When one output size is enough, prefer decoding
	/// straight to it ([`Config::resize`]), which is free on decoders with a
	/// hardware scaler; this method is for fanning one decoded stream out to
	/// several sizes.
	pub fn resize(&self, size: Size) -> Result<Frame, Error> {
		size.validate("resize to")?;
		let Size { width, height } = size;

		let inner = match &self.inner {
			crate::frame::Frame::I420(i420) => crate::frame::Frame::I420(i420.resize(width, height)?),
			#[cfg(all(target_os = "linux", feature = "nvdec"))]
			crate::frame::Frame::Cuda(cuda) => match cuda.resize(width, height) {
				Ok(scaled) => crate::frame::Frame::Cuda(scaled),
				// E.g. the driver rejected the vendored PTX: degrade to a CPU
				// resize (download once) instead of killing the stream.
				Err(err) => {
					static WARN_ONCE: std::sync::Once = std::sync::Once::new();
					WARN_ONCE.call_once(|| tracing::warn!(%err, "GPU resize failed; falling back to the CPU"));
					crate::frame::Frame::I420(cuda.download_i420()?.resize(width, height)?)
				}
			},
			// CVPixelBuffer (the VideoToolbox decoder's output) and D3D11 textures
			// have no GPU scaler wired up yet: download, then scale on the CPU.
			#[allow(unreachable_patterns)]
			other => crate::frame::Frame::I420(other.to_i420()?.into_owned().resize(width, height)?),
		};

		Ok(Frame {
			timestamp: self.timestamp,
			size,
			inner,
		})
	}

	/// The frame as tightly-packed I420 (YUV 4:2:0, BT.601 limited range): Y
	/// (`width * height` bytes), then U, then V (`width/2 * height/2` bytes
	/// each), no row padding. Free for a CPU frame; downloads a GPU frame.
	///
	/// Consumes the frame: transcoders should instead pass it to
	/// [`encode::Encoder::encode`](crate::encode::Encoder::encode), which keeps
	/// a GPU frame on the GPU.
	pub fn into_i420(self) -> Result<Bytes, Error> {
		match self.inner {
			crate::frame::Frame::I420(i420) => Ok(Bytes::from(i420.data)),
			// GPU frames (CUDA / CVPixelBuffer / D3D11): download and pack.
			#[allow(unreachable_patterns)]
			other => Ok(Bytes::from(other.to_i420()?.into_owned().data)),
		}
	}

	/// This frame as a CoreVideo pixel buffer, the mirror of
	/// [`into_i420`](Self::into_i420) pointing the other way.
	///
	/// Free for a hardware-decoded frame: it is already a `CVPixelBuffer`, so this
	/// is a retain, and the picture never leaves the GPU. A CPU frame (the software
	/// decoder's output) is uploaded into a fresh buffer instead, so this always
	/// gives you something to draw rather than making you write the upload
	/// yourself. Wrap the result in a `CVMetalTextureCache` to render it.
	///
	/// Check `CVPixelBufferGetPixelFormatType` before sampling: a hardware decode
	/// hands back NV12 (bi-planar), while an uploaded CPU frame is planar I420.
	///
	/// A hardware-decoded buffer comes from the decoder's pool, so holding many
	/// frames holds pool slots and eventually stalls decoding. Draw and drop.
	///
	/// Requires the `surface` feature. See [`objc2_core_video`] for the version
	/// coupling that implies.
	#[cfg(all(feature = "surface", target_os = "macos"))]
	pub fn into_pixel_buffer(
		self,
	) -> Result<objc2_core_foundation::CFRetained<objc2_core_video::CVPixelBuffer>, Error> {
		match self.inner {
			crate::frame::Frame::Surface(surface) => Ok(surface.buffer),
			crate::frame::Frame::I420(i420) => crate::frame::macos::upload_i420(&i420),
		}
	}
}

#[cfg(test)]
mod tests {
	/// Callers (libmoq, moq-transcode) hold these across `.await`s in spawned
	/// tasks and share frames via `Arc` (the transcode fanout), so both must
	/// stay `Send` and `Frame` also `Sync` even when a platform's frame wraps
	/// a GPU handle. Compile-time check; fails per-platform if a variant
	/// regresses.
	#[test]
	fn frame_and_consumer_are_thread_safe() {
		fn assert_send<T: Send>() {}
		fn assert_sync<T: Sync>() {}
		assert_send::<super::Frame>();
		assert_sync::<super::Frame>();
		assert_send::<super::Consumer>();
	}

	/// `into_pixel_buffer` is total: a CPU frame uploads rather than failing, so a
	/// renderer never has to write the upload itself. Software-decoded frames take
	/// this path.
	#[cfg(all(feature = "surface", target_os = "macos"))]
	#[test]
	fn into_pixel_buffer_uploads_a_cpu_frame() {
		use objc2_core_video::{CVPixelBufferGetHeight, CVPixelBufferGetWidth};

		let frame = super::Frame {
			timestamp: moq_net::Timestamp::from_micros(0).unwrap(),
			size: crate::Size::new(64, 32),
			inner: crate::frame::Frame::I420(crate::frame::I420 {
				width: 64,
				height: 32,
				data: vec![0x80; crate::frame::I420::len(64, 32)],
			}),
		};

		let buffer = frame.into_pixel_buffer().expect("upload a CPU frame");
		assert_eq!(CVPixelBufferGetWidth(&buffer), 64);
		assert_eq!(CVPixelBufferGetHeight(&buffer), 32);
	}
}
