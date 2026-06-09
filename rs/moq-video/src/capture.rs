//! Webcam capture via libavdevice.
//!
//! Opens the platform camera backend (avfoundation on macOS, v4l2 on
//! Linux, dshow on Windows) and yields decoded [`ffmpeg::frame::Video`]
//! frames in the camera's native pixel format. The [`Encoder`](crate::Encoder)
//! handles conversion to YUV420P, so callers don't have to care what the
//! camera delivers.

use std::ffi::CString;

use ffmpeg_next as ffmpeg;

use crate::Error;

/// Webcam capture configuration. All fields are hints; the backend picks
/// the closest supported mode.
#[derive(Clone, Debug, Default)]
pub struct CameraConfig {
	/// Platform device identifier. `None` opens the default camera.
	///
	/// - macOS (avfoundation): device index (`"0"`) or name (`"FaceTime HD Camera"`).
	/// - Linux (v4l2): a `/dev/videoN` path.
	/// - Windows (dshow): the device name (without the `video=` prefix).
	pub device: Option<String>,
	pub width: Option<u32>,
	pub height: Option<u32>,
	pub framerate: Option<u32>,
}

/// An open camera, read frame-by-frame via [`read`](Self::read).
pub struct Camera {
	input: ffmpeg::format::context::Input,
	decoder: ffmpeg::decoder::Video,
	stream_index: usize,
	url: String,
}

impl Camera {
	/// Open the camera described by `config`.
	pub fn open(config: &CameraConfig) -> Result<Self, Error> {
		ffmpeg::init()?;
		ffmpeg::device::register_all();

		let backend = Backend::current();
		let url = backend.url(config.device.as_deref());

		let input_format = find_input_format(backend.format_name)?;
		let mut opts = ffmpeg::Dictionary::new();
		if let (Some(w), Some(h)) = (config.width, config.height) {
			opts.set("video_size", &format!("{w}x{h}"));
		}
		if let Some(fps) = config.framerate {
			opts.set("framerate", &fps.to_string());
		}

		let ctx = ffmpeg::format::open_with(&url, &input_format, opts)?;
		let input = match ctx {
			ffmpeg::format::context::Context::Input(input) => input,
			ffmpeg::format::context::Context::Output(_) => {
				// open_with returns Input for an Input format; this arm is unreachable.
				return Err(Error::NoVideoStream(url));
			}
		};

		let stream = input
			.streams()
			.best(ffmpeg::media::Type::Video)
			.ok_or_else(|| Error::NoVideoStream(url.clone()))?;
		let stream_index = stream.index();

		let decoder = ffmpeg::codec::context::Context::from_parameters(stream.parameters())?
			.decoder()
			.video()?;

		tracing::info!(
			device = %url,
			backend = backend.format_name,
			width = decoder.width(),
			height = decoder.height(),
			"opened camera"
		);

		Ok(Self {
			input,
			decoder,
			stream_index,
			url,
		})
	}

	/// Native pixel format the camera decodes to.
	pub fn format(&self) -> ffmpeg::format::Pixel {
		self.decoder.format()
	}

	pub fn width(&self) -> u32 {
		self.decoder.width()
	}

	pub fn height(&self) -> u32 {
		self.decoder.height()
	}

	/// Block until the next decoded frame is available, or `None` once the
	/// device stops producing frames.
	pub fn read(&mut self) -> Result<Option<ffmpeg::frame::Video>, Error> {
		let mut frame = ffmpeg::frame::Video::empty();
		loop {
			match self.decoder.receive_frame(&mut frame) {
				Ok(()) => return Ok(Some(frame)),
				Err(ffmpeg::Error::Other { errno }) if errno == ffmpeg::util::error::EAGAIN => {}
				Err(ffmpeg::Error::Eof) => return Ok(None),
				Err(e) => return Err(e.into()),
			}

			// Pull the next packet for our stream. The inner block drops the
			// packet iterator (and its borrow of `input`) before we touch
			// `decoder`, keeping the borrow checker happy.
			let packet = {
				let mut packets = self.input.packets();
				loop {
					match packets.next() {
						Some((stream, packet)) if stream.index() == self.stream_index => break Some(packet),
						Some(_) => continue,
						None => break None,
					}
				}
			};

			match packet {
				Some(packet) => self.decoder.send_packet(&packet)?,
				None => {
					self.decoder.send_eof()?;
					return match self.decoder.receive_frame(&mut frame) {
						Ok(()) => Ok(Some(frame)),
						// Drained: no more frames after EOF.
						Err(ffmpeg::Error::Eof) => Ok(None),
						Err(ffmpeg::Error::Other { errno }) if errno == ffmpeg::util::error::EAGAIN => Ok(None),
						// A real decode failure must not masquerade as end-of-stream.
						Err(e) => Err(e.into()),
					};
				}
			}
		}
	}

	pub fn device(&self) -> &str {
		&self.url
	}
}

/// Platform capture backend selection.
struct Backend {
	format_name: &'static str,
}

impl Backend {
	#[cfg(target_os = "macos")]
	fn current() -> Self {
		Self {
			format_name: "avfoundation",
		}
	}

	#[cfg(target_os = "linux")]
	fn current() -> Self {
		Self { format_name: "v4l2" }
	}

	#[cfg(target_os = "windows")]
	fn current() -> Self {
		Self { format_name: "dshow" }
	}

	#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
	fn current() -> Self {
		Self {
			format_name: "avfoundation",
		}
	}

	/// Build the libavdevice URL for the requested device.
	fn url(&self, device: Option<&str>) -> String {
		match self.format_name {
			// avfoundation device spec is "<video>:<audio>"; pin audio to
			// "none" so we only open the camera, never a microphone.
			"avfoundation" => {
				let video = device.unwrap_or("default");
				if video.contains(':') {
					video.to_string()
				} else {
					format!("{video}:none")
				}
			}
			"v4l2" => device.unwrap_or("/dev/video0").to_string(),
			"dshow" => format!("video={}", device.unwrap_or("")),
			_ => device.unwrap_or("default").to_string(),
		}
	}
}

/// Look up a libavdevice input format by name. The safe `format::list()`
/// helper is compiled out on ffmpeg >= 5, so we go through the FFI.
fn find_input_format(name: &str) -> Result<ffmpeg::format::format::Format, Error> {
	let cname = CString::new(name).expect("format name has no interior NUL");
	// SAFETY: `av_find_input_format` takes a NUL-terminated string and returns
	// a borrowed static pointer (or null). We check for null before wrapping.
	let ptr = unsafe { ffmpeg::ffi::av_find_input_format(cname.as_ptr()) };
	if ptr.is_null() {
		return Err(match name {
			"avfoundation" => Error::NoCaptureBackend("avfoundation"),
			"v4l2" => Error::NoCaptureBackend("v4l2"),
			"dshow" => Error::NoCaptureBackend("dshow"),
			_ => Error::NoCaptureBackend("camera"),
		});
	}
	// SAFETY: `ptr` is a non-null `AVInputFormat` owned statically by
	// libavdevice; the const->mut cast is sound because `Input` never mutates
	// through it (the wrapper only reads format fields).
	let input = unsafe { ffmpeg::format::Input::wrap(ptr as *mut _) };
	Ok(ffmpeg::format::format::Format::Input(input))
}
