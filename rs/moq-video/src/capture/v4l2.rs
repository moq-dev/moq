//! Native V4L2 webcam capture (Linux), replacing nokhwa.
//!
//! Streams MMAP buffers through the [`v4l`] crate and converts each frame to CPU
//! [`I420`] for the encoder. Two source formats cover essentially all UVC
//! webcams: YUYV (raw 4:2:2, resampled directly) and MJPEG (decoded to RGB with
//! the pure-Rust [`zune_jpeg`], then converted). This is the CPU path feeding
//! NVENC / VAAPI / openh264; there's no GPU surface here.

use v4l::buffer::Type as BufType;
use v4l::capability::Flags;
use v4l::io::mmap::Stream as MmapStream;
use v4l::io::traits::CaptureStream;
use v4l::video::Capture;
use v4l::video::capture::Parameters;
use v4l::{Device, Format, FourCC};
use zune_jpeg::zune_core::bytestream::ZCursor;

use super::channel::FrameChannel;
use super::pump::{self, Geometry};
use super::{Config, Stream};
use crate::frame::{I420, Surface};
use crate::{Error, Size};

/// List V4L2 capture nodes using paths that [`open_device`] accepts.
pub(super) fn cameras() -> Result<Vec<super::Camera>, Error> {
	let mut nodes = v4l::context::enum_devices();
	nodes.sort_by_key(v4l::context::Node::index);

	let cameras = nodes
		.into_iter()
		.filter_map(|node| {
			let path = node.path().to_string_lossy().into_owned();
			let device = match Device::with_path(node.path()) {
				Ok(device) => device,
				Err(err) => {
					tracing::debug!(device = %path, error = %err, "could not inspect V4L2 node");
					return None;
				}
			};
			let capabilities = match device.query_caps() {
				Ok(capabilities) => capabilities,
				Err(err) => {
					tracing::debug!(device = %path, error = %err, "could not query V4L2 node");
					return None;
				}
			};
			if !capabilities.capabilities.contains(Flags::VIDEO_CAPTURE)
				|| !capabilities.capabilities.contains(Flags::STREAMING)
			{
				return None;
			}

			let name = node.name().filter(|name| !name.is_empty()).unwrap_or(capabilities.card);
			Some(super::Camera { id: path, name })
		})
		.collect();
	Ok(cameras)
}

/// Open a V4L2 camera and stream its frames over a pump thread.
pub(super) async fn open(config: &Config, device: Option<&str>) -> Result<Stream, Error> {
	let config = config.clone();
	// The camera opens on the pump thread, so the selector has to be owned.
	let device = device.map(str::to_string);
	let chan = FrameChannel::new();
	let (geo, guard) = pump::spawn(
		chan.clone(),
		move || {
			let camera = Camera::open(&config, device.as_deref())?;
			let geometry = Geometry {
				width: camera.width,
				height: camera.height,
				framerate: camera.framerate,
				label: camera.name.clone(),
			};
			Ok((camera, geometry))
		},
		Camera::read,
	)
	.await?;

	Ok(Stream::new(
		chan,
		geo.width,
		geo.height,
		geo.framerate,
		geo.label,
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

/// The negotiated source format, chosen once at open.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Source {
	/// Raw 4:2:2; resampled to I420 with no color-space conversion.
	Yuyv,
	/// Motion-JPEG; decoded per frame.
	Mjpeg,
}

impl Source {
	/// Every format we can convert, cheapest first. The order only breaks ties
	/// between modes that fit the requested size equally well.
	const ALL: [Self; 2] = [Self::Yuyv, Self::Mjpeg];

	fn fourcc(self) -> FourCC {
		FourCC::new(match self {
			Self::Yuyv => b"YUYV",
			Self::Mjpeg => b"MJPG",
		})
	}

	fn from_fourcc(fourcc: FourCC) -> Option<Self> {
		Self::ALL.into_iter().find(|source| source.fourcc() == fourcc)
	}

	fn cost(self) -> u8 {
		match self {
			Self::Yuyv => 0,
			Self::Mjpeg => 1,
		}
	}
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
	fn open(config: &Config, selector: Option<&str>) -> Result<Self, Error> {
		let (device, name) = open_device(selector)?;
		let width = config.width.unwrap_or(DEFAULT_WIDTH);
		let height = config.height.unwrap_or(DEFAULT_HEIGHT);

		let (format, source) = negotiate(&device, &name, Size::new(width, height))?;

		let (width, height, stride) = (format.width, format.height, format.stride);
		Size::new(width, height).validate("camera resolution")?;

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
	fn read(&mut self) -> Result<pump::Read, Error> {
		let (buf, meta) = CaptureStream::next(&mut self.stream)
			.map_err(|error| Error::SourceUnavailable(format!("V4L2 camera {}: {error}", self.name)))?;

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
				// The stream reports the negotiated size and the encoder is built
				// from it, so a frame that decodes to another one can't be published
				// as this stream's.
				if w as u32 != self.width || h as u32 != self.height {
					return Err(Error::Codec(anyhow::anyhow!(
						"MJPEG frame is {w}x{h}, not the negotiated {}x{}",
						self.width,
						self.height
					)));
				}
				I420::from_rgb(&rgb, self.width, self.height)?
			}
		};
		Ok(pump::Read::Frame(Surface::I420(i420)))
	}
}

/// Open `device`: a bare integer selects `/dev/videoN` by index, anything
/// else is a device path. `None` opens index 0.
fn open_device(device: Option<&str>) -> Result<(Device, String), Error> {
	match device {
		None => {
			let device = Device::new(0).map_err(|error| open_error("/dev/video0", error))?;
			Ok((device, "/dev/video0".to_string()))
		}
		Some(spec) => match spec.parse::<usize>() {
			Ok(index) => {
				let name = format!("/dev/video{index}");
				let device = Device::new(index).map_err(|error| open_error(&name, error))?;
				Ok((device, format!("/dev/video{index}")))
			}
			Err(_) => {
				let device = Device::with_path(spec).map_err(|error| open_error(spec, error))?;
				Ok((device, spec.to_string()))
			}
		},
	}
}

fn open_error(device: &str, error: std::io::Error) -> Error {
	match error.kind() {
		std::io::ErrorKind::PermissionDenied => Error::PermissionDenied(format!("{device}: {error}")),
		_ => Error::SourceUnavailable(format!("{device}: {error}")),
	}
}

/// Negotiate the format we can convert to I420 that lands closest to `want`.
///
/// V4L2's non-mutating `VIDIOC_TRY_FMT` is optional, so use the required
/// `VIDIOC_S_FMT`. It asks and applies in one step, substituting the driver's
/// nearest supported mode for anything it doesn't have. Each format we handle
/// is applied in turn and scored against the requested geometry, then the
/// winner is applied again to leave the device on it.
///
/// Taking the first reply instead would pin most laptop webcams to VGA: USB
/// bandwidth doesn't fit uncompressed 4:2:2 above that, so they offer YUYV only
/// at small sizes and reach HD through MJPEG alone. Asking such a camera for
/// YUYV at 1080p gets 640x480 back, which is a valid YUYV mode and nowhere near
/// what the caller asked for.
fn negotiate(device: &Device, name: &str, want: Size) -> Result<(Format, Source), Error> {
	negotiate_with(name, want, |format| set_format(device, format))
}

fn negotiate_with(
	name: &str,
	want: Size,
	mut apply: impl FnMut(Format) -> Result<Format, Error>,
) -> Result<(Format, Source), Error> {
	let mut replies = Vec::with_capacity(Source::ALL.len());
	let mut offered = Vec::new();
	let mut probe_error = None;
	for candidate in Source::ALL {
		let got = match apply(Format::new(want.width, want.height, candidate.fourcc())) {
			Ok(got) => got,
			Err(error) => {
				probe_error = Some(error);
				continue;
			}
		};
		let description = format!("{}x{} {}", got.width, got.height, got.fourcc);
		if !offered.contains(&description) {
			offered.push(description);
		}
		if let Some(source) = Source::from_fourcc(got.fourcc) {
			replies.push((got, source));
		}
	}

	let Some((best, source)) = closest(replies, want) else {
		if offered.is_empty() {
			let Some(error) = probe_error else {
				return Err(Error::Codec(anyhow::anyhow!("camera {name} has no formats to probe")));
			};
			return Err(error);
		}
		let offered = offered.join(", ");
		let wanted = Source::ALL.map(|source| source.fourcc().to_string()).join(", ");
		return Err(Error::Codec(anyhow::anyhow!(
			"camera {name} has no encodable {wanted} mode (the driver returned {offered})"
		)));
	};

	// A successful probe may have left the device on another candidate.
	let applied = apply(Format::new(best.width, best.height, best.fourcc))?;
	if applied.fourcc != best.fourcc || applied.width != best.width || applied.height != best.height {
		return Err(Error::Codec(anyhow::anyhow!(
			"camera {name} would not re-apply the {}x{} {} mode it just negotiated",
			best.width,
			best.height,
			best.fourcc
		)));
	}
	Ok((applied, source))
}

/// The encodable reply nearest the requested geometry, ties going to the cheaper
/// returned format.
fn closest(replies: impl IntoIterator<Item = (Format, Source)>, want: Size) -> Option<(Format, Source)> {
	replies
		.into_iter()
		.filter(|(format, _)| {
			Size::new(format.width, format.height)
				.validate("camera resolution")
				.is_ok()
		})
		.min_by_key(|(format, source)| (distance(*format, want), source.cost()))
}

/// How far a negotiated mode lands from the requested geometry, summed over both
/// dimensions. Zero is an exact match.
fn distance(format: Format, want: Size) -> u64 {
	u64::from(format.width.abs_diff(want.width)) + u64::from(format.height.abs_diff(want.height))
}

fn set_format(device: &Device, format: Format) -> Result<Format, Error> {
	Capture::set_format(device, &format).map_err(|e| Error::Codec(anyhow::anyhow!("V4L2 set format: {e}")))
}

#[cfg(test)]
mod tests {
	use super::*;

	fn reply(width: u32, height: u32, source: Source) -> (Format, Source) {
		(Format::new(width, height, source.fourcc()), source)
	}

	/// The case that motivates scoring at all, taken from a real UVC webcam:
	/// YUYV tops out at VGA while MJPEG reaches 720p, so a 720p request has to
	/// land on MJPEG even though YUYV is probed first and answers successfully.
	#[test]
	fn prefers_the_nearer_mode_over_the_cheaper_one() {
		let replies = [reply(640, 480, Source::Yuyv), reply(1280, 720, Source::Mjpeg)];
		let (format, source) = closest(replies, Size::new(1280, 720)).expect("a reply is usable");
		assert_eq!(source, Source::Mjpeg);
		assert_eq!((format.width, format.height), (1280, 720));
	}

	/// When both formats reach the requested size, the cheaper one wins regardless
	/// of probe order: YUYV resamples, MJPEG costs a full JPEG decode per frame.
	#[test]
	fn breaks_ties_toward_the_cheaper_format() {
		let replies = [reply(640, 480, Source::Mjpeg), reply(640, 480, Source::Yuyv)];
		let (_, source) = closest(replies, Size::new(640, 480)).expect("a reply is usable");
		assert_eq!(source, Source::Yuyv);
	}

	/// An exact odd mode cannot feed I420, so a nearby even mode has to win rather
	/// than letting `Camera::open` reject the selected result.
	#[test]
	fn ignores_a_nearer_mode_the_pipeline_cannot_encode() {
		let replies = [reply(1279, 719, Source::Mjpeg), reply(1280, 720, Source::Yuyv)];
		let (format, source) = closest(replies, Size::new(1279, 719)).expect("an even reply is usable");
		assert_eq!(source, Source::Yuyv);
		assert_eq!((format.width, format.height), (1280, 720));
	}

	/// A YUYV-only driver may reject MJPEG instead of substituting its supported
	/// mode, which must not discard the valid reply from the first probe.
	#[test]
	fn keeps_a_valid_mode_when_another_probe_fails() {
		let mut calls = 0;
		let (format, source) = negotiate_with("camera", Size::new(640, 480), |requested| {
			calls += 1;
			match calls {
				1 => Ok(Format::new(640, 480, Source::Yuyv.fourcc())),
				2 => Err(Error::Codec(anyhow::anyhow!("MJPEG is unsupported"))),
				3 => Ok(requested),
				_ => panic!("unexpected format probe"),
			}
		})
		.expect("the YUYV reply is usable");

		assert_eq!(calls, 3);
		assert_eq!(source, Source::Yuyv);
		assert_eq!((format.width, format.height), (640, 480));
	}

	/// When every candidate is rejected, preserve the last real driver error.
	#[test]
	fn returns_an_error_when_every_probe_fails() {
		let mut calls = 0;
		let error = negotiate_with("camera", Size::new(640, 480), |_| {
			calls += 1;
			Err(Error::Codec(anyhow::anyhow!("probe {calls} failed")))
		})
		.expect_err("no format probe succeeded");

		assert_eq!(calls, Source::ALL.len());
		assert_eq!(error.to_string(), "probe 2 failed");
	}

	/// Zero and odd dimensions cannot feed I420, so they leave nothing to score.
	#[test]
	fn no_usable_reply_is_none() {
		let replies = [reply(0, 720, Source::Yuyv), reply(1279, 719, Source::Mjpeg)];
		assert!(closest(replies, Size::new(1280, 720)).is_none());
	}

	/// Distance is symmetric in the two dimensions and zero only on an exact hit,
	/// so a mode that overshoots is no better than one that undershoots by as much.
	#[test]
	fn distance_is_zero_only_on_an_exact_match() {
		let want = Size::new(1280, 720);
		assert_eq!(distance(Format::new(1280, 720, Source::Yuyv.fourcc()), want), 0);
		assert_eq!(distance(Format::new(1280, 600, Source::Yuyv.fourcc()), want), 120);
		assert_eq!(distance(Format::new(1280, 840, Source::Yuyv.fourcc()), want), 120);
	}

	/// Every format we advertise round-trips through its fourcc, which is what
	/// lets `negotiate` recognize the driver's substitution.
	#[test]
	fn fourcc_round_trips() {
		for source in Source::ALL {
			assert_eq!(Source::from_fourcc(source.fourcc()), Some(source));
		}
		assert_eq!(Source::from_fourcc(FourCC::new(b"GREY")), None);
	}
}
