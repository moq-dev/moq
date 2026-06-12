//! Webcam capture via [`nokhwa`] (non-macOS): decodes MJPEG/YUYV/NV12 to RGBA,
//! which the encoder converts to I420. The CPU path, used until a native
//! zero-copy capture lands on these platforms.

use nokhwa::Camera as NokhwaCamera;
use nokhwa::pixel_format::RgbAFormat;
use nokhwa::utils::{CameraFormat, CameraIndex, FrameFormat, RequestedFormat, RequestedFormatType, Resolution};

use super::{Config, FrameSource};
use crate::Error;
use crate::frame::{Frame, I420};

/// Default framerate hint when one is requested but unspecified.
const DEFAULT_FRAMERATE: u32 = 30;

/// An open camera, read frame-by-frame via [`read`](Self::read).
pub(crate) struct Camera {
	inner: NokhwaCamera,
	width: u32,
	height: u32,
	framerate: Option<u32>,
	device: String,
}

impl Camera {
	pub(super) fn open(config: &Config) -> Result<Self, Error> {
		let index = camera_index(config.device.as_deref());

		// Prefer the requested resolution; fall back to the highest available if
		// the camera can't honor the hint (e.g. no matching MJPEG mode).
		let mut camera = match requested_format(config) {
			Some(requested) => NokhwaCamera::new(index.clone(), requested)
				.or_else(|_| NokhwaCamera::new(index.clone(), highest()))
				.map_err(|e| Error::Codec(anyhow::anyhow!("open camera: {e}")))?,
			None => NokhwaCamera::new(index.clone(), highest())
				.map_err(|e| Error::Codec(anyhow::anyhow!("open camera: {e}")))?,
		};

		camera
			.open_stream()
			.map_err(|e| Error::Codec(anyhow::anyhow!("start camera stream: {e}")))?;

		let resolution = camera.resolution();
		let (width, height) = (resolution.width(), resolution.height());
		let framerate = match camera.frame_rate() {
			0 => None,
			fps => Some(fps),
		};
		let device = match &index {
			CameraIndex::Index(i) => i.to_string(),
			CameraIndex::String(s) => s.clone(),
		};

		tracing::info!(device = %device, width, height, framerate, "opened camera");

		Ok(Self {
			inner: camera,
			width,
			height,
			framerate,
			device,
		})
	}
}

impl FrameSource for Camera {
	fn read(&mut self) -> Result<Option<Frame>, Error> {
		let buffer = self
			.inner
			.frame()
			.map_err(|e| Error::Codec(anyhow::anyhow!("capture frame: {e}")))?;
		let image = buffer
			.decode_image::<RgbAFormat>()
			.map_err(|e| Error::Codec(anyhow::anyhow!("decode frame: {e}")))?;

		let (width, height) = (image.width(), image.height());
		Ok(Some(Frame::I420(I420::from_rgba(&image.into_raw(), width, height))))
	}

	fn width(&self) -> u32 {
		self.width
	}

	fn height(&self) -> u32 {
		self.height
	}

	fn framerate(&self) -> Option<u32> {
		self.framerate
	}

	fn device(&self) -> &str {
		&self.device
	}
}

fn camera_index(device: Option<&str>) -> CameraIndex {
	match device {
		Some(d) => match d.parse::<u32>() {
			Ok(i) => CameraIndex::Index(i),
			Err(_) => CameraIndex::String(d.to_string()),
		},
		None => CameraIndex::Index(0),
	}
}

/// A resolution-pinned request, or `None` when no size hint was given.
fn requested_format(config: &Config) -> Option<RequestedFormat<'static>> {
	let (Some(width), Some(height)) = (config.width, config.height) else {
		return None;
	};
	let format = CameraFormat::new(
		Resolution::new(width, height),
		FrameFormat::MJPEG,
		config.framerate.unwrap_or(DEFAULT_FRAMERATE),
	);
	Some(RequestedFormat::new::<RgbAFormat>(RequestedFormatType::Closest(format)))
}

fn highest() -> RequestedFormat<'static> {
	RequestedFormat::new::<RgbAFormat>(RequestedFormatType::AbsoluteHighestResolution)
}
