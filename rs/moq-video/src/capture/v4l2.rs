//! Zero-copy V4L2 capture: NV12 driver buffers exported as dmabufs and handed to
//! the VAAPI encoder ([`Frame::DmaBuf`]) without a CPU copy.
//!
//! Lifecycle: `VIDIOC_REQBUFS` reserves MMAP buffers; `VIDIOC_EXPBUF` exports each
//! one as a persistent dmabuf fd (kept for the session); every buffer is queued
//! and streaming starts. Each [`read`](FrameSource::read) dequeues a filled
//! buffer, hands out a dup of that buffer's dmabuf fd, and re-queues the buffer
//! the consumer held *last* time. Re-queuing one frame late is safe because the
//! capture/encode loop is synchronous and single-threaded: by the time `read` is
//! called again, the encoder has finished the previous frame. Holding one buffer
//! out at a time is why we reserve several.
//!
//! NOT YET RUNTIME-VALIDATED. It compiles on Linux but has not run against a real
//! `/dev/video*` device; the EXPBUF flags, NV12 plane layout, and re-queue timing
//! need a Linux + VAAPI box to confirm end to end. The higher-level device open
//! and format negotiation go through the `v4l` crate; only the streaming ioctls
//! it doesn't wrap (EXPBUF, manual QBUF/DQBUF) are issued directly.

use std::ffi::c_void;
use std::fs::File;
use std::mem;
use std::os::fd::FromRawFd;

use v4l::buffer::Type as BufType;
use v4l::device::Device;
use v4l::memory::Memory;
use v4l::v4l_sys::{v4l2_buffer, v4l2_exportbuffer, v4l2_requestbuffers};
use v4l::video::Capture;
use v4l::video::capture::Parameters;
use v4l::{Format, FourCC};

use super::{Config, FrameSource};
use crate::Error;
use crate::frame::Frame;
use crate::frame::linux::{DmaPlane, DmaSurface};

/// DRM/V4L2 four-character code we negotiate. NV12 is the format VAAPI imports
/// and the cros-codecs encoder is configured for.
const FOURCC_NV12: &[u8; 4] = b"NV12";

/// Driver buffers to reserve. We only ever hold one out at a time, but a small
/// ring lets the device keep filling while the encoder works on the last frame.
const BUFFER_COUNT: u32 = 4;

/// Fallback geometry when the caller doesn't pin a resolution; the driver may
/// still negotiate something close.
const DEFAULT_WIDTH: u32 = 1280;
const DEFAULT_HEIGHT: u32 = 720;

pub(crate) struct Camera {
	device: Device,
	width: u32,
	height: u32,
	/// Bytes per row of the Y plane (`bytesperline`); the interleaved UV plane
	/// shares it for NV12.
	stride: usize,
	framerate: Option<u32>,
	name: String,
	/// One persistent exported dmabuf per reserved buffer index. Each `read`
	/// hands out a dup so the underlying buffer can be re-queued independently of
	/// the fd the encoder still holds.
	dmabufs: Vec<File>,
	/// Buffer index handed to the consumer on the previous `read`, re-queued at
	/// the start of the next one. `None` before the first frame.
	inflight: Option<u32>,
}

impl Camera {
	pub(crate) fn open(config: &Config) -> Result<Self, Error> {
		let (device, name) = open_device(config)?;

		// Negotiate NV12 at the requested (or default) geometry. The driver
		// returns what it actually picked, which is what we size everything to.
		let requested = Format::new(
			config.width.unwrap_or(DEFAULT_WIDTH),
			config.height.unwrap_or(DEFAULT_HEIGHT),
			FourCC::new(FOURCC_NV12),
		);
		let format = Capture::set_format(&device, &requested)
			.map_err(|e| Error::Codec(anyhow::anyhow!("V4L2 set NV12: {e}")))?;
		if &format.fourcc.repr != FOURCC_NV12 {
			return Err(Error::Codec(anyhow::anyhow!(
				"V4L2 device {name} does not support NV12 (negotiated {})",
				format.fourcc
			)));
		}

		// Best-effort framerate request; many devices clamp or ignore it.
		if let Some(fps) = config.framerate {
			let _ = Capture::set_params(&device, &Parameters::with_fps(fps));
		}
		let framerate = Capture::params(&device).ok().and_then(|p| {
			// interval is seconds-per-frame (num/denom), so fps = denom/num.
			(p.interval.numerator != 0).then(|| (p.interval.denominator / p.interval.numerator).max(1))
		});

		let width = format.width;
		let height = format.height;
		let stride = (format.stride.max(width)) as usize;

		let dmabufs = export_buffers(&device, BUFFER_COUNT)?;
		for index in 0..dmabufs.len() as u32 {
			queue_buffer(&device, index)?;
		}
		stream_on(&device)?;

		tracing::info!(device = %name, width, height, "opened V4L2 dmabuf capture");
		Ok(Self {
			device,
			width,
			height,
			stride,
			framerate,
			name,
			dmabufs,
			inflight: None,
		})
	}
}

impl FrameSource for Camera {
	fn read(&mut self) -> Result<Option<Frame>, Error> {
		// Return the previous frame's buffer to the driver now that the encoder
		// has consumed it (the loop calling us is synchronous).
		if let Some(index) = self.inflight.take() {
			queue_buffer(&self.device, index)?;
		}

		// The device fd is O_NONBLOCK; wait for a filled buffer before DQBUF.
		let ready = self
			.device
			.handle()
			.poll(libc::POLLIN, -1)
			.map_err(|e| Error::Codec(anyhow::anyhow!("V4L2 poll: {e}")))?;
		if ready == 0 {
			return Ok(None);
		}

		let mut buf = buffer_desc();
		// SAFETY: `buf` is a zeroed v4l2_buffer with type/memory set, the layout
		// VIDIOC_DQBUF expects; the driver fills in `index` and metadata.
		unsafe { ioctl(&self.device, v4l::v4l2::vidioc::VIDIOC_DQBUF, &mut buf) }
			.map_err(|e| Error::Codec(anyhow::anyhow!("V4L2 DQBUF: {e}")))?;
		let index = buf.index;

		let dmabuf = self.dmabufs[index as usize]
			.try_clone()
			.map_err(|e| Error::Codec(anyhow::anyhow!("dup dmabuf fd: {e}")))?;
		self.inflight = Some(index);

		// NV12 is a single buffer: Y plane at offset 0, interleaved UV right
		// after it, both at `stride` bytes per row.
		let chroma_offset = self.stride * self.height as usize;
		Ok(Some(Frame::DmaBuf(DmaSurface {
			fds: vec![dmabuf],
			width: self.width,
			height: self.height,
			fourcc: *FOURCC_NV12,
			modifier: 0, // DRM_FORMAT_MOD_LINEAR
			planes: vec![
				DmaPlane {
					buffer_index: 0,
					offset: 0,
					stride: self.stride,
				},
				DmaPlane {
					buffer_index: 0,
					offset: chroma_offset,
					stride: self.stride,
				},
			],
		})))
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
		&self.name
	}
}

impl Drop for Camera {
	fn drop(&mut self) {
		let mut typ = BufType::VideoCapture as u32;
		// SAFETY: stop streaming on the still-open device fd; ignore errors since
		// we're tearing down (the fd and exported dmabufs close right after).
		unsafe {
			let _ = ioctl_raw(
				&self.device,
				v4l::v4l2::vidioc::VIDIOC_STREAMOFF,
				&mut typ as *mut u32 as *mut c_void,
			);
		}
	}
}

/// Open `config.device`: a bare integer selects `/dev/videoN` by index, anything
/// else is treated as a device path. `None` opens index 0.
fn open_device(config: &Config) -> Result<(Device, String), Error> {
	match config.device.as_deref() {
		None => {
			let device = Device::new(0).map_err(|e| Error::Codec(anyhow::anyhow!("open /dev/video0: {e}")))?;
			Ok((device, "/dev/video0".to_string()))
		}
		Some(spec) => {
			if let Ok(index) = spec.parse::<usize>() {
				let device =
					Device::new(index).map_err(|e| Error::Codec(anyhow::anyhow!("open /dev/video{index}: {e}")))?;
				Ok((device, format!("/dev/video{index}")))
			} else {
				let device = Device::with_path(spec).map_err(|e| Error::Codec(anyhow::anyhow!("open {spec}: {e}")))?;
				Ok((device, spec.to_string()))
			}
		}
	}
}

/// Reserve `count` MMAP buffers and export each as a persistent dmabuf fd.
fn export_buffers(device: &Device, count: u32) -> Result<Vec<File>, Error> {
	let mut req = v4l2_requestbuffers {
		count,
		type_: BufType::VideoCapture as u32,
		memory: Memory::Mmap as u32,
		// SAFETY: zero the reserved tail; the three fields above are the ABI
		// inputs VIDIOC_REQBUFS reads.
		..unsafe { mem::zeroed() }
	};
	unsafe { ioctl(device, v4l::v4l2::vidioc::VIDIOC_REQBUFS, &mut req) }
		.map_err(|e| Error::Codec(anyhow::anyhow!("V4L2 REQBUFS: {e}")))?;
	if req.count == 0 {
		return Err(Error::Codec(anyhow::anyhow!("V4L2 device granted no buffers")));
	}

	let mut bufs = Vec::with_capacity(req.count as usize);
	for index in 0..req.count {
		let mut exp = v4l2_exportbuffer {
			type_: BufType::VideoCapture as u32,
			index,
			// Read-only is enough for VAAPI import; CLOEXEC so the fd doesn't
			// leak across exec. (Flag choice to confirm on real hardware.)
			flags: (libc::O_RDONLY | libc::O_CLOEXEC) as u32,
			..unsafe { mem::zeroed() }
		};
		unsafe { ioctl(device, v4l::v4l2::vidioc::VIDIOC_EXPBUF, &mut exp) }
			.map_err(|e| Error::Codec(anyhow::anyhow!("V4L2 EXPBUF index {index}: {e}")))?;
		// SAFETY: on success EXPBUF returns a fresh owned fd in `exp.fd`.
		bufs.push(unsafe { File::from_raw_fd(exp.fd) });
	}
	Ok(bufs)
}

fn queue_buffer(device: &Device, index: u32) -> Result<(), Error> {
	let mut buf = v4l2_buffer { index, ..buffer_desc() };
	unsafe { ioctl(device, v4l::v4l2::vidioc::VIDIOC_QBUF, &mut buf) }
		.map_err(|e| Error::Codec(anyhow::anyhow!("V4L2 QBUF index {index}: {e}")))?;
	Ok(())
}

fn stream_on(device: &Device) -> Result<(), Error> {
	let mut typ = BufType::VideoCapture as u32;
	unsafe { ioctl(device, v4l::v4l2::vidioc::VIDIOC_STREAMON, &mut typ) }
		.map_err(|e| Error::Codec(anyhow::anyhow!("V4L2 STREAMON: {e}")))?;
	Ok(())
}

/// A zeroed single-planar capture `v4l2_buffer` with type + MMAP memory set.
fn buffer_desc() -> v4l2_buffer {
	v4l2_buffer {
		type_: BufType::VideoCapture as u32,
		memory: Memory::Mmap as u32,
		// SAFETY: the remaining fields are plain integers / a union the driver
		// fills or ignores for a capture-side MMAP buffer.
		..unsafe { mem::zeroed() }
	}
}

/// Typed wrapper over the raw V4L2 ioctl: passes `&mut arg` as the `argp`.
///
/// # Safety
/// `arg` must be the struct the kernel associates with `request` (e.g. a
/// `v4l2_buffer` for QBUF/DQBUF), laid out per the V4L2 ABI.
unsafe fn ioctl<T>(device: &Device, request: v4l::v4l2::vidioc::_IOC_TYPE, arg: &mut T) -> std::io::Result<()> {
	unsafe { ioctl_raw(device, request, arg as *mut T as *mut c_void) }
}

unsafe fn ioctl_raw(device: &Device, request: v4l::v4l2::vidioc::_IOC_TYPE, argp: *mut c_void) -> std::io::Result<()> {
	unsafe { v4l::v4l2::ioctl(device.handle().fd(), request, argp) }
}
