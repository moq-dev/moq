//! Native V4L2 webcam capture (Linux), replacing nokhwa.
//!
//! Streams MMAP buffers through the [`v4l`] crate and converts each frame to CPU
//! [`I420`] for the encoder. Two source formats cover essentially all UVC
//! webcams: YUYV (raw 4:2:2, resampled directly) and MJPEG (decoded to RGB with
//! the pure-Rust [`zune_jpeg`], then converted). This is the CPU path feeding
//! NVENC / VAAPI / openh264; there's no GPU surface here.

use v4l::buffer::Type as BufType;
use v4l::io::mmap::Stream as MmapStream;
use v4l::io::traits::CaptureStream;
use v4l::video::Capture;
use v4l::video::capture::Parameters;
use v4l::{Device, Format, FourCC};
use zune_jpeg::zune_core::bytestream::ZCursor;

use super::channel::FrameChannel;
use super::pump::{self, Geometry};
use super::{Config, FrameStream};
use crate::Error;
use crate::frame::{Frame, I420};

/// Open a V4L2 camera and stream its frames over a pump thread.
pub(super) async fn open(config: &Config) -> Result<FrameStream, Error> {
	let config = config.clone();
	let chan = FrameChannel::new();
	let (geo, guard) = pump::spawn(
		chan.clone(),
		move || {
			let camera = Camera::open(&config)?;
			let geometry = Geometry {
				width: camera.width,
				height: camera.height,
				framerate: camera.framerate,
				device: camera.name.clone(),
			};
			Ok((camera, geometry))
		},
		Camera::read,
	)
	.await?;

	Ok(FrameStream::new(
		chan,
		geo.width,
		geo.height,
		geo.framerate,
		geo.device,
		None,
		Box::new(guard),
	))
}

/// Fallback geometry when the caller doesn't pin a resolution; the driver picks
/// the nearest mode it supports.
const DEFAULT_WIDTH: u32 = 1280;
const DEFAULT_HEIGHT: u32 = 720;

/// Driver buffers to keep in flight; a small ring lets capture overlap encode.
const BUFFER_COUNT: u32 = 4;

/// Raw 4:2:2; resampled to I420 with no color-space conversion.
const FOURCC_YUYV: &[u8; 4] = b"YUYV";
/// Motion-JPEG; decoded per frame.
const FOURCC_MJPG: &[u8; 4] = b"MJPG";

/// The negotiated source format, chosen once at open.
#[derive(Clone, Copy)]
enum Source {
	Yuyv,
	Mjpeg,
}

pub(crate) struct Camera {
	stream: MmapStream<'static>,
	source: Source,
	width: u32,
	height: u32,
	/// Bytes per row of the YUYV buffer (`bytesperline`); unused for MJPEG.
	stride: u32,
	framerate: Option<u32>,
	name: String,
}

impl Camera {
	fn open(config: &Config) -> Result<Self, Error> {
		let (device, name) = open_device(config)?;
		let width = config.width.unwrap_or(DEFAULT_WIDTH);
		let height = config.height.unwrap_or(DEFAULT_HEIGHT);

		let format = negotiate(&device, width, height)?;
		let source = match &format.fourcc.repr {
			f if f == FOURCC_YUYV => Source::Yuyv,
			f if f == FOURCC_MJPG => Source::Mjpeg,
			other => {
				return Err(Error::Codec(anyhow::anyhow!(
					"camera {name} supports neither YUYV nor MJPEG (negotiated {})",
					FourCC::new(other)
				)));
			}
		};

		let (width, height, stride) = (format.width, format.height, format.stride);
		// I420 chroma is 2x2 subsampled, so the encoder needs even dimensions.
		if width % 2 != 0 || height % 2 != 0 {
			return Err(Error::Codec(anyhow::anyhow!(
				"camera resolution {width}x{height} must be even for H.264 encoding"
			)));
		}

		// Best-effort framerate request; many cameras clamp or ignore it.
		if let Some(fps) = config.framerate {
			let _ = Capture::set_params(&device, &Parameters::with_fps(fps));
		}
		let framerate = Capture::params(&device).ok().and_then(|p| {
			// interval is seconds-per-frame (num/denom), so fps = denom/num.
			(p.interval.numerator != 0).then(|| (p.interval.denominator / p.interval.numerator).max(1))
		});

		// The stream owns a clone of the device's `Arc<Handle>`, so the fd stays
		// open after `device` drops here; the mmap'd buffers live with the stream.
		let stream = MmapStream::with_buffers(&device, BufType::VideoCapture, BUFFER_COUNT)
			.map_err(|e| Error::Codec(anyhow::anyhow!("V4L2 stream init: {e}")))?;

		tracing::info!(device = %name, width, height, "opened V4L2 capture");
		Ok(Self {
			stream,
			source,
			width,
			height,
			stride,
			framerate,
			name,
		})
	}

	/// Pull the next frame. Blocks one frame interval; the pump thread calls this
	/// in a loop and checks its stop flag between calls.
	fn read(&mut self) -> Result<Option<Frame>, Error> {
		let (buf, meta) =
			CaptureStream::next(&mut self.stream).map_err(|e| Error::Codec(anyhow::anyhow!("V4L2 capture: {e}")))?;

		let i420 = match self.source {
			Source::Yuyv => I420::from_yuyv(buf, self.stride, self.width, self.height)?,
			Source::Mjpeg => {
				// Only `bytesused` of the buffer holds the JPEG; the rest is stale.
				let jpeg = buf.get(..meta.bytesused as usize).unwrap_or(buf);
				// zune-jpeg 0.5 reads through a seekable cursor, not a bare slice.
				let mut decoder = zune_jpeg::JpegDecoder::new(ZCursor::new(jpeg));
				let rgb = decoder
					.decode()
					.map_err(|e| Error::Codec(anyhow::anyhow!("MJPEG decode: {e:?}")))?;
				let (w, h) = decoder
					.dimensions()
					.ok_or_else(|| Error::Codec(anyhow::anyhow!("MJPEG frame had no dimensions")))?;
				I420::from_rgb(&rgb, w as u32, h as u32)?
			}
		};
		Ok(Some(Frame::I420(i420)))
	}
}

/// Open `config.device`: a bare integer selects `/dev/videoN` by index, anything
/// else is a device path. `None` opens index 0.
fn open_device(config: &Config) -> Result<(Device, String), Error> {
	match config.device.as_deref() {
		None => {
			let device = Device::new(0).map_err(|e| Error::Codec(anyhow::anyhow!("open /dev/video0: {e}")))?;
			Ok((device, "/dev/video0".to_string()))
		}
		Some(spec) => match spec.parse::<usize>() {
			Ok(index) => {
				let device =
					Device::new(index).map_err(|e| Error::Codec(anyhow::anyhow!("open /dev/video{index}: {e}")))?;
				Ok((device, format!("/dev/video{index}")))
			}
			Err(_) => {
				let device = Device::with_path(spec).map_err(|e| Error::Codec(anyhow::anyhow!("open {spec}: {e}")))?;
				Ok((device, spec.to_string()))
			}
		},
	}
}

/// Negotiate a format we can convert to I420. We ask for YUYV then MJPEG; the
/// driver substitutes its nearest supported pixel format, so accept the first
/// reply that lands on one we handle.
fn negotiate(device: &Device, width: u32, height: u32) -> Result<Format, Error> {
	let mut last = None;
	for want in [FOURCC_YUYV, FOURCC_MJPG] {
		let requested = Format::new(width, height, FourCC::new(want));
		let got = Capture::set_format(device, &requested)
			.map_err(|e| Error::Codec(anyhow::anyhow!("V4L2 set format: {e}")))?;
		if &got.fourcc.repr == FOURCC_YUYV || &got.fourcc.repr == FOURCC_MJPG {
			return Ok(got);
		}
		last = Some(got.fourcc);
	}
	Err(Error::Codec(anyhow::anyhow!(
		"camera supports neither YUYV nor MJPEG (negotiated {last:?})"
	)))
}
