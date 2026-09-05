//! Shared V4L2 memory-to-memory device plumbing for the Linux hardware codec
//! backends.
//!
//! M2M is the kernel interface an ARM SoC exposes its video codec through: one
//! device node with two queues, OUTPUT for what userspace feeds in and CAPTURE
//! for what the hardware hands back. An encoder takes raw frames on OUTPUT and
//! returns an elementary stream on CAPTURE; a decoder runs the same node the
//! other way around. Both backends drive it through the [`Device`] and [`Queue`]
//! here, so the ioctl sequence, the buffer pool, and the plane arithmetic are
//! written once.
//!
//! Built on the raw layer of the `v4l` crate that the camera capture path
//! already uses: `v4l::v4l_sys` supplies the `videodev2.h` structs and
//! `v4l::v4l2::vidioc` the request codes, so nothing here hand-rolls a struct
//! offset.
//!
//! The driver, not the caller, decides the raw layout. `VIDIOC_S_FMT` is a
//! negotiation, and the [`Format`] that comes back can carry a different fourcc,
//! a wider stride, and more rows than were asked for. [`Planes`] reads that
//! answer and is the only thing either backend consults about where chroma
//! lives.

use std::collections::VecDeque;
use std::fs::File;
use std::os::fd::AsRawFd;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::ptr::NonNull;
use std::time::Duration;

use v4l::v4l_sys::{
	V4L2_BUF_FLAG_ERROR, V4L2_BUF_FLAG_LAST, V4L2_CAP_DEVICE_CAPS, V4L2_CAP_STREAMING, V4L2_CAP_VIDEO_M2M,
	V4L2_CAP_VIDEO_M2M_MPLANE, V4L2_EVENT_SOURCE_CHANGE, V4L2_SEL_TGT_COMPOSE, timeval,
	v4l2_buf_type_V4L2_BUF_TYPE_VIDEO_CAPTURE_MPLANE, v4l2_buf_type_V4L2_BUF_TYPE_VIDEO_OUTPUT_MPLANE, v4l2_buffer,
	v4l2_capability, v4l2_colorspace_V4L2_COLORSPACE_REC709, v4l2_colorspace_V4L2_COLORSPACE_SMPTE170M, v4l2_control,
	v4l2_decoder_cmd, v4l2_encoder_cmd, v4l2_event, v4l2_event_subscription, v4l2_field_V4L2_FIELD_NONE, v4l2_fmtdesc,
	v4l2_format, v4l2_memory_V4L2_MEMORY_MMAP, v4l2_plane, v4l2_quantization_V4L2_QUANTIZATION_FULL_RANGE,
	v4l2_quantization_V4L2_QUANTIZATION_LIM_RANGE, v4l2_requestbuffers, v4l2_selection, v4l2_streamparm,
	v4l2_xfer_func_V4L2_XFER_FUNC_709, v4l2_ycbcr_encoding_V4L2_YCBCR_ENC_601, v4l2_ycbcr_encoding_V4L2_YCBCR_ENC_709,
};
use v4l::v4l2::vidioc;

use crate::frame::I420;
use crate::{Color, Error, Size};

/// A V4L2 fourcc, which is its four ASCII bytes little-endian.
///
/// `videodev2.h` spells these with a macro, so bindgen emits none of them.
const fn fourcc(code: [u8; 4]) -> u32 {
	u32::from_le_bytes(code)
}

/// Semi-planar 4:2:0, luma then interleaved chroma, in one buffer.
pub(crate) const NV12: u32 = fourcc(*b"NV12");
/// The same layout with each plane in its own buffer.
pub(crate) const NV12M: u32 = fourcc(*b"NM12");
/// Planar 4:2:0, luma then U then V, in one buffer.
pub(crate) const YUV420: u32 = fourcc(*b"YU12");
/// The same layout with each plane in its own buffer.
pub(crate) const YUV420M: u32 = fourcc(*b"YM12");
/// An H.264 elementary stream, one access unit per buffer.
pub(crate) const H264: u32 = fourcc(*b"H264");

/// The raw formats either backend can read and write, in preference order.
///
/// All four are 8-bit 4:2:0, which is what `Surface::I420` converts to and from
/// without resampling. Which one a driver picks is its business: the Raspberry
/// Pi's `bcm2835-codec` answers an NV12 request with `YU12` on some firmware
/// revisions, and [`Planes`] handles whichever comes back.
pub(crate) const RAW: &[u32] = &[NV12, NV12M, YUV420, YUV420M];

/// Planes a format may use here: Y, U, and V.
const MAX_PLANES: usize = 3;

/// Requests the `v4l` crate does not export, built the way
/// `linux/ioctl.h` builds them. The shifts are `asm-generic`'s, which is what
/// `v4l`'s own table already assumes.
mod request {
	use v4l::v4l_sys::{v4l2_decoder_cmd, v4l2_event, v4l2_event_subscription, v4l2_selection};
	use v4l::v4l2::vidioc::_IOC_TYPE;

	const READ: u32 = 2;
	const WRITE: u32 = 1;

	const fn code(dir: u32, nr: u32, size: usize) -> _IOC_TYPE {
		((dir as _IOC_TYPE) << 30) | ((size as _IOC_TYPE) << 16) | ((b'V' as _IOC_TYPE) << 8) | nr as _IOC_TYPE
	}

	pub(super) const DQEVENT: _IOC_TYPE = code(READ, 89, size_of::<v4l2_event>());
	pub(super) const SUBSCRIBE_EVENT: _IOC_TYPE = code(WRITE, 90, size_of::<v4l2_event_subscription>());
	pub(super) const G_SELECTION: _IOC_TYPE = code(READ | WRITE, 94, size_of::<v4l2_selection>());
	pub(super) const DECODER_CMD: _IOC_TYPE = code(READ | WRITE, 96, size_of::<v4l2_decoder_cmd>());

	#[cfg(test)]
	mod tests {
		use v4l::v4l_sys::{v4l2_capability, v4l2_format};
		use v4l::v4l2::vidioc;

		use super::*;

		/// The codes above have to come out the way `linux/ioctl.h` builds
		/// the rest, and nothing else here would notice if they did not: a wrong
		/// code is `ENOTTY` at runtime on whichever board reaches it first.
		///
		/// Checked against requests the `v4l` crate does export, one per direction,
		/// so the shifts, the direction bits, and the struct size all have to agree
		/// on whatever target this compiles for.
		#[test]
		fn the_codes_are_built_the_way_the_crate_builds_its_own() {
			assert_eq!(code(READ, 0, size_of::<v4l2_capability>()), vidioc::VIDIOC_QUERYCAP);
			assert_eq!(code(WRITE, 18, size_of::<std::ffi::c_int>()), vidioc::VIDIOC_STREAMON);
			assert_eq!(code(READ | WRITE, 5, size_of::<v4l2_format>()), vidioc::VIDIOC_S_FMT);
			// `v4l` 0.14 omits this newer request. This is the value from
			// `videodev2.h`, and also checks the generated command struct's ABI size.
			assert_eq!(DECODER_CMD, 0xc048_5660);
		}
	}
}

/// A `videodev2.h` struct an ioctl reads or fills.
///
/// # Safety
///
/// The type must be plain data with no niche, so that an all-zero value is a
/// valid one. Every struct in `videodev2.h` is, which is what lets each request
/// start from zero: the kernel wants the fields it reserves left that way.
unsafe trait Arg: Sized {
	/// A zeroed value, the starting point for every request here.
	fn zeroed() -> Self {
		// SAFETY: the implementer promises all-zero is a valid value.
		unsafe { std::mem::zeroed() }
	}
}

// SAFETY: each is a `videodev2.h` struct: integers, arrays, and unions of the
// same, with no reference, no enum, and no niche.
unsafe impl Arg for v4l2_buffer {}
unsafe impl Arg for v4l2_capability {}
unsafe impl Arg for v4l2_decoder_cmd {}
unsafe impl Arg for v4l2_encoder_cmd {}
unsafe impl Arg for v4l2_event {}
unsafe impl Arg for v4l2_event_subscription {}
unsafe impl Arg for v4l2_fmtdesc {}
unsafe impl Arg for v4l2_format {}
unsafe impl Arg for v4l2_requestbuffers {}
unsafe impl Arg for v4l2_selection {}
unsafe impl Arg for v4l2_streamparm {}
unsafe impl Arg for [v4l2_plane; MAX_PLANES] {}

/// Which queue of an M2M device, named the way V4L2 names them: from the
/// driver's point of view, not ours.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Dir {
	/// What userspace feeds the device: raw frames to an encoder, coded bytes to
	/// a decoder.
	Output,
	/// What the device hands back: coded bytes from an encoder, raw frames from a
	/// decoder.
	Capture,
}

impl Dir {
	fn buf_type(self) -> u32 {
		match self {
			Dir::Output => v4l2_buf_type_V4L2_BUF_TYPE_VIDEO_OUTPUT_MPLANE,
			Dir::Capture => v4l2_buf_type_V4L2_BUF_TYPE_VIDEO_CAPTURE_MPLANE,
		}
	}
}

/// What to ask a queue for. The driver answers with a [`Format`] that may differ
/// in every field.
pub(crate) struct Request {
	/// The fourcc to ask for.
	pub pixelformat: u32,
	/// The picture size to ask for.
	pub size: Size,
	/// Buffer size to ask for, which a compressed queue needs because the driver
	/// cannot size an access unit from the picture dimensions. `None` leaves the
	/// driver's own estimate, which is what a raw queue wants.
	pub sizeimage: Option<u32>,
	/// The color space of the samples. An encoder driver that fills the VUI reads
	/// it from here, so the bitstream says what the pixels actually are.
	pub color: Option<Color>,
}

/// One plane of a negotiated [`Format`].
#[derive(Clone, Copy, Debug)]
pub(crate) struct Plane {
	/// Bytes between the starts of adjacent rows, at least the row's width and
	/// usually more.
	pub stride: u32,
	/// Bytes the driver wants this plane's buffer to be, alignment padding
	/// included.
	pub sizeimage: u32,
}

/// The format a queue negotiated, read back from the driver's `VIDIOC_S_FMT`
/// answer rather than assumed from the request.
#[derive(Clone, Debug)]
pub(crate) struct Format {
	/// The fourcc the driver chose, which need not be the one asked for.
	pub pixelformat: u32,
	/// The coded size, at least the requested one and rounded up to whatever the
	/// hardware aligns to.
	pub size: Size,
	/// One entry per plane, in plane order.
	pub planes: Vec<Plane>,
}

/// How to recognize the M2M node for one codec, since node numbering is per SoC.
pub(crate) struct Role {
	/// Environment variable naming a node directly, for a driver whose format
	/// enumeration doesn't match what it can actually do.
	pub env: &'static str,
	/// Fourccs the OUTPUT queue has to accept, any one of them.
	pub input: &'static [u32],
	/// Fourccs the CAPTURE queue has to produce, any one of them.
	pub output: &'static [u32],
}

/// Open the M2M node for `role`, searching every V4L2 node for one that converts
/// what the role names.
///
/// The path is per SoC (`/dev/video11` encodes on a Raspberry Pi, and other SoCs
/// number theirs differently), so the node is identified by what it converts
/// rather than by a table of guesses. `role.env` overrides the search.
pub(crate) fn open(role: &Role) -> Result<Device, Error> {
	if let Ok(path) = std::env::var(role.env) {
		let device = Device::open(Path::new(&path))?;
		device.check(role)?;
		return Ok(device);
	}

	let mut nodes = v4l::context::enum_devices();
	nodes.sort_by_key(v4l::context::Node::index);

	let mut refused = Vec::new();
	for node in nodes {
		let path = node.path();
		match Device::open(path).and_then(|device| device.check(role).map(|()| device)) {
			Ok(device) => return Ok(device),
			// Most nodes on a box are cameras or metadata nodes, so a miss is the
			// common case and only interesting when the search finds nothing.
			Err(err) => refused.push(format!("{}: {err}", path.display())),
		}
	}

	Err(Error::Codec(anyhow::anyhow!(
		"no V4L2 M2M node converts {} to {} (set {} to name one; tried {})",
		join_fourcc(role.input),
		join_fourcc(role.output),
		role.env,
		match refused.is_empty() {
			true => "no nodes".to_owned(),
			false => refused.join(", "),
		}
	)))
}

/// Render fourccs as their ASCII spelling for an error message.
fn join_fourcc(codes: &[u32]) -> String {
	codes.iter().copied().map(name).collect::<Vec<_>>().join("/")
}

/// The ASCII spelling of a fourcc, for logs and errors.
pub(crate) fn name(code: u32) -> String {
	String::from_utf8_lossy(&code.to_le_bytes()).into_owned()
}

/// An open V4L2 M2M device node.
pub(crate) struct Device {
	file: File,
	path: PathBuf,
}

impl Device {
	/// Open `path`, failing unless it reports a multi-planar M2M capability.
	fn open(path: &Path) -> Result<Self, Error> {
		// Non-blocking, so a `VIDIOC_DQBUF` with nothing ready returns `EAGAIN`
		// instead of parking the caller's thread. Waiting is explicit, through
		// `wait`, and always bounded.
		let file = std::fs::OpenOptions::new()
			.read(true)
			.write(true)
			.custom_flags(libc::O_NONBLOCK)
			.open(path)
			.map_err(|err| Error::Codec(anyhow::anyhow!("{}: open: {err}", path.display())))?;

		let device = Self {
			file,
			path: path.to_path_buf(),
		};

		let caps = device.capabilities()?;
		if caps & V4L2_CAP_VIDEO_M2M_MPLANE == 0 {
			// Single-planar M2M exists, but no codec driver ships only that, and
			// supporting it would double every queue path below for no hardware.
			let what = match caps & V4L2_CAP_VIDEO_M2M {
				0 => "not an M2M device",
				_ => "single-planar M2M only",
			};
			return Err(Error::Codec(anyhow::anyhow!("{}: {what}", path.display())));
		}
		// Everything below streams MMAP buffers, so a node without streaming I/O is
		// the wrong node however its formats enumerate.
		if caps & V4L2_CAP_STREAMING == 0 {
			return Err(Error::Codec(anyhow::anyhow!("{}: no streaming I/O", path.display())));
		}
		Ok(device)
	}

	/// Whether this node converts what `role` names.
	fn check(&self, role: &Role) -> Result<(), Error> {
		for (dir, wanted) in [(Dir::Output, role.input), (Dir::Capture, role.output)] {
			let offered = self.formats(dir)?;
			if !wanted.iter().any(|code| offered.contains(code)) {
				return Err(Error::Codec(anyhow::anyhow!(
					"offers {} where {} is needed",
					join_fourcc(&offered),
					join_fourcc(wanted)
				)));
			}
		}
		Ok(())
	}

	/// The node path, for logs.
	pub(crate) fn path(&self) -> &Path {
		&self.path
	}

	/// Issue `request` against this node.
	///
	/// # Safety
	///
	/// `arg` must be the struct type `request` was defined for.
	unsafe fn ioctl<T>(&self, request: vidioc::_IOC_TYPE, arg: &mut T) -> std::io::Result<()> {
		// SAFETY: the caller guarantees `arg` matches `request`, and the pointer
		// stays valid for the call, which is the only thing the kernel needs.
		unsafe { v4l::v4l2::ioctl(self.file.as_raw_fd(), request, (arg as *mut T).cast()) }
	}

	/// Wrap an ioctl failure with the node and the operation that failed.
	fn err(&self, what: impl std::fmt::Display, err: std::io::Error) -> Error {
		Error::Codec(anyhow::anyhow!("{}: {what}: {err}", self.path.display()))
	}

	/// The capabilities of this node specifically, rather than of the driver as a
	/// whole. A codec driver registers several nodes off one `v4l2_capability`,
	/// so only `device_caps` says what *this* one does.
	fn capabilities(&self) -> Result<u32, Error> {
		let mut caps = v4l2_capability::zeroed();
		// SAFETY: `VIDIOC_QUERYCAP` takes a `v4l2_capability`.
		unsafe { self.ioctl(vidioc::VIDIOC_QUERYCAP, &mut caps) }.map_err(|err| self.err("QUERYCAP", err))?;

		Ok(match caps.capabilities & V4L2_CAP_DEVICE_CAPS {
			0 => caps.capabilities,
			_ => caps.device_caps,
		})
	}

	/// Every fourcc a queue offers, from `VIDIOC_ENUM_FMT`.
	///
	/// A decoder's CAPTURE answer narrows once the stream has been parsed: what
	/// comes back then is what the driver supports for that stream, which is a
	/// subset of what it offered at open.
	pub(crate) fn formats(&self, dir: Dir) -> Result<Vec<u32>, Error> {
		let mut formats = Vec::new();
		for index in 0.. {
			// The enumeration ends with `EINVAL`, which is not an error here. Any
			// other failure is, but reporting it as an empty list would only turn a
			// broken node into a confusing "offers nothing", so stop either way.
			let mut desc = v4l2_fmtdesc::zeroed();
			desc.index = index;
			desc.type_ = dir.buf_type();
			// SAFETY: `VIDIOC_ENUM_FMT` takes a `v4l2_fmtdesc`.
			if unsafe { self.ioctl(vidioc::VIDIOC_ENUM_FMT, &mut desc) }.is_err() {
				break;
			}
			formats.push(desc.pixelformat);
		}
		Ok(formats)
	}

	/// Negotiate a queue's format, returning what the driver settled on.
	///
	/// `VIDIOC_G_FMT` runs first because the struct carries fields this does not
	/// set, colorimetry defaults among them, and `bcm2835-codec` rejects an
	/// `S_FMT` built from zeroes.
	pub(crate) fn set_format(&self, dir: Dir, request: &Request) -> Result<Format, Error> {
		let mut format = v4l2_format::zeroed();
		format.type_ = dir.buf_type();
		// SAFETY: `VIDIOC_G_FMT` takes a `v4l2_format`.
		unsafe { self.ioctl(vidioc::VIDIOC_G_FMT, &mut format) }.map_err(|err| self.err("G_FMT", err))?;

		{
			// SAFETY: the queue type is one of the `_MPLANE` pair, so the union
			// holds `pix_mp`.
			let pix = unsafe { &mut format.fmt.pix_mp };
			pix.width = request.size.width;
			pix.height = request.size.height;
			pix.pixelformat = request.pixelformat;
			pix.field = v4l2_field_V4L2_FIELD_NONE;
			// The driver raises this for a format whose planes live in separate
			// buffers; asking for one plane is right for every contiguous format and
			// harmless for the rest.
			pix.num_planes = 1;
			// Zero is how V4L2 asks the driver to size a plane itself. Left as
			// `G_FMT` reported them, these would be the stride and buffer size of
			// whatever format the queue held before this call, which is the wrong
			// answer for any request that changes the dimensions.
			for plane in &mut pix.plane_fmt {
				plane.bytesperline = 0;
				plane.sizeimage = 0;
			}
			if let Some(sizeimage) = request.sizeimage {
				pix.plane_fmt[0].sizeimage = sizeimage;
			}
			if let Some(color) = request.color {
				let (colorspace, ycbcr) = match color {
					// SMPTE 170M primaries and matrix with the BT.709 transfer curve,
					// the same triple the other backends emit: the two curves are
					// defined identically and only 709 has a name everywhere.
					Color::Bt601Limited | Color::Bt601Full => (
						v4l2_colorspace_V4L2_COLORSPACE_SMPTE170M,
						v4l2_ycbcr_encoding_V4L2_YCBCR_ENC_601,
					),
					Color::Bt709Limited | Color::Bt709Full => (
						v4l2_colorspace_V4L2_COLORSPACE_REC709,
						v4l2_ycbcr_encoding_V4L2_YCBCR_ENC_709,
					),
				};
				pix.colorspace = colorspace;
				pix.__bindgen_anon_1.ycbcr_enc = ycbcr as u8;
				pix.xfer_func = v4l2_xfer_func_V4L2_XFER_FUNC_709 as u8;
				pix.quantization = match color.limited() {
					true => v4l2_quantization_V4L2_QUANTIZATION_LIM_RANGE as u8,
					false => v4l2_quantization_V4L2_QUANTIZATION_FULL_RANGE as u8,
				};
			}
		}

		// SAFETY: `VIDIOC_S_FMT` takes a `v4l2_format`.
		unsafe { self.ioctl(vidioc::VIDIOC_S_FMT, &mut format) }.map_err(|err| self.err("S_FMT", err))?;
		self.read_format(&format)
	}

	fn read_format(&self, format: &v4l2_format) -> Result<Format, Error> {
		// SAFETY: only ever called on a struct whose type is one of the `_MPLANE`
		// pair, so the union holds `pix_mp`.
		let pix = unsafe { &format.fmt.pix_mp };
		let count = (pix.num_planes as usize).clamp(1, MAX_PLANES);
		Ok(Format {
			pixelformat: pix.pixelformat,
			size: Size::new(pix.width, pix.height),
			planes: pix.plane_fmt[..count]
				.iter()
				.map(|plane| Plane {
					stride: plane.bytesperline,
					sizeimage: plane.sizeimage,
				})
				.collect(),
		})
	}

	/// Read a queue's current format without changing it, which is how a decoder
	/// learns the picture size the stream turned out to have.
	pub(crate) fn format(&self, dir: Dir) -> Result<Format, Error> {
		let mut format = v4l2_format::zeroed();
		format.type_ = dir.buf_type();
		// SAFETY: `VIDIOC_G_FMT` takes a `v4l2_format`.
		unsafe { self.ioctl(vidioc::VIDIOC_G_FMT, &mut format) }.map_err(|err| self.err("G_FMT", err))?;
		self.read_format(&format)
	}

	/// The visible rectangle of a decoded picture, or `None` from a driver that
	/// does not report one.
	///
	/// Coding happens in macroblocks, so a stream's coded size is rounded up and
	/// the picture people are meant to see is the compose rectangle inside it:
	/// 1080p codes as 1088 rows, of which the last 8 are not part of the picture.
	/// The rectangle has an origin as well as a size, since H.264 can crop any
	/// edge, so the offset is kept rather than assumed to be the corner.
	pub(crate) fn visible(&self, dir: Dir) -> Option<Rect> {
		let mut selection = v4l2_selection::zeroed();
		selection.type_ = dir.buf_type();
		selection.target = V4L2_SEL_TGT_COMPOSE;
		// SAFETY: `VIDIOC_G_SELECTION` takes a `v4l2_selection`.
		unsafe { self.ioctl(request::G_SELECTION, &mut selection) }.ok()?;

		let rect = Rect {
			left: selection.r.left.max(0) as u32,
			top: selection.r.top.max(0) as u32,
			size: Size::new(selection.r.width, selection.r.height),
		};
		match rect.size.width == 0 || rect.size.height == 0 {
			true => None,
			false => Some(rect),
		}
	}

	/// Ask the driver to report resolution changes, which is the only way a
	/// stateful decoder announces the picture size.
	pub(crate) fn subscribe_source_change(&self) -> Result<(), Error> {
		let mut subscription = v4l2_event_subscription::zeroed();
		subscription.type_ = V4L2_EVENT_SOURCE_CHANGE;
		// SAFETY: `VIDIOC_SUBSCRIBE_EVENT` takes a `v4l2_event_subscription`.
		unsafe { self.ioctl(request::SUBSCRIBE_EVENT, &mut subscription) }
			.map_err(|err| self.err("SUBSCRIBE_EVENT", err))
	}

	/// Drain the event queue, reporting whether a source change was among them.
	///
	/// Every pending event is taken rather than one, since a caller that read
	/// them one per call would fall behind a driver that queues several.
	pub(crate) fn take_source_change(&self) -> bool {
		let mut changed = false;
		loop {
			let mut event = v4l2_event::zeroed();
			// SAFETY: `VIDIOC_DQEVENT` takes a `v4l2_event`.
			if unsafe { self.ioctl(request::DQEVENT, &mut event) }.is_err() {
				return changed;
			}
			changed |= event.type_ == V4L2_EVENT_SOURCE_CHANGE;
		}
	}

	/// Read a control the driver computes rather than one we set.
	pub(crate) fn control(&self, id: u32) -> Result<i32, Error> {
		let mut control = v4l2_control { id, value: 0 };
		// SAFETY: `VIDIOC_G_CTRL` takes a `v4l2_control`.
		unsafe { self.ioctl(vidioc::VIDIOC_G_CTRL, &mut control) }
			.map_err(|err| self.err(format_args!("G_CTRL {id:#x}"), err))?;
		Ok(control.value)
	}

	/// Declare the input framerate, which rate control uses to spend the bitrate.
	pub(crate) fn set_framerate(&self, dir: Dir, framerate: u32) -> Result<(), Error> {
		let mut parm = v4l2_streamparm::zeroed();
		parm.type_ = dir.buf_type();
		// SAFETY: the buffer type just written picks the arm the driver reads, so
		// the two have to agree: a CAPTURE queue's parameters are `capture` and an
		// OUTPUT queue's are `output`.
		let time_per_frame = unsafe {
			match dir {
				Dir::Output => &mut parm.parm.output.timeperframe,
				Dir::Capture => &mut parm.parm.capture.timeperframe,
			}
		};
		time_per_frame.numerator = 1;
		time_per_frame.denominator = framerate;

		// SAFETY: `VIDIOC_S_PARM` takes a `v4l2_streamparm`.
		unsafe { self.ioctl(vidioc::VIDIOC_S_PARM, &mut parm) }.map_err(|err| self.err("S_PARM", err))
	}

	/// Issue a `VIDIOC_ENCODER_CMD`, which is how an encoder is told to drain and
	/// then to resume.
	///
	/// # Errors
	///
	/// `EINVAL` or `ENOTTY` from a driver that implements no encoder commands,
	/// which the caller has to be able to carry on without.
	pub(crate) fn encoder_cmd(&self, cmd: u32) -> Result<(), Error> {
		// The drain sequence wants `flags` and `pts` zero, which is where every
		// request starts.
		let mut command = v4l2_encoder_cmd::zeroed();
		command.cmd = cmd;
		// SAFETY: `VIDIOC_ENCODER_CMD` takes a `v4l2_encoder_cmd`.
		unsafe { self.ioctl(vidioc::VIDIOC_ENCODER_CMD, &mut command) }
			.map_err(|err| self.err(format_args!("ENCODER_CMD {cmd}"), err))
	}

	/// Issue a `VIDIOC_DECODER_CMD`, which drains or resumes a stateful decoder.
	pub(crate) fn decoder_cmd(&self, cmd: u32) -> Result<(), Error> {
		// The stateful drain sequence wants `flags` and `pts` zero, which is where
		// every request starts.
		let mut command = v4l2_decoder_cmd::zeroed();
		command.cmd = cmd;
		// SAFETY: `VIDIOC_DECODER_CMD` takes a `v4l2_decoder_cmd`.
		unsafe { self.ioctl(request::DECODER_CMD, &mut command) }
			.map_err(|err| self.err(format_args!("DECODER_CMD {cmd}"), err))
	}

	/// Set a codec control, failing if the driver rejects it.
	pub(crate) fn set_control(&self, id: u32, value: i32) -> Result<(), Error> {
		let mut control = v4l2_control { id, value };
		// SAFETY: `VIDIOC_S_CTRL` takes a `v4l2_control`.
		unsafe { self.ioctl(vidioc::VIDIOC_S_CTRL, &mut control) }
			.map_err(|err| self.err(format_args!("S_CTRL {id:#x} = {value}"), err))
	}

	/// Set a control the driver is allowed not to have.
	///
	/// Optional by design rather than by accident: the two ways to ask for
	/// repeated parameter sets are each supported by only some drivers, so both
	/// are offered and whichever lands wins.
	pub(crate) fn try_control(&self, id: u32, value: i32) -> bool {
		match self.set_control(id, value) {
			Ok(()) => true,
			Err(err) => {
				tracing::debug!(control = format!("{id:#x}"), value, %err, "V4L2 control not supported");
				false
			}
		}
	}

	/// Allocate `count` mmap buffers for a queue, returning how many the driver
	/// actually gave.
	fn request_buffers(&self, dir: Dir, count: u32) -> Result<u32, Error> {
		let mut request = v4l2_requestbuffers::zeroed();
		request.count = count;
		request.type_ = dir.buf_type();
		request.memory = v4l2_memory_V4L2_MEMORY_MMAP;

		// SAFETY: `VIDIOC_REQBUFS` takes a `v4l2_requestbuffers`.
		unsafe { self.ioctl(vidioc::VIDIOC_REQBUFS, &mut request) }
			.map_err(|err| self.err(format_args!("REQBUFS {count}"), err))?;
		Ok(request.count)
	}

	fn stream(&self, dir: Dir, on: bool) -> Result<(), Error> {
		let mut buf_type = dir.buf_type();
		let request = match on {
			true => vidioc::VIDIOC_STREAMON,
			false => vidioc::VIDIOC_STREAMOFF,
		};
		// SAFETY: both take an `int` holding the buffer type.
		unsafe { self.ioctl(request, &mut buf_type) }.map_err(|err| {
			let what = match on {
				true => "STREAMON",
				false => "STREAMOFF",
			};
			self.err(what, err)
		})
	}

	/// Park until the device has something to hand over, or `timeout` elapses.
	///
	/// Blocking, which is what the caller is after: every codec backend here runs
	/// on its own OS thread, so nothing on an executor is parked.
	///
	/// Buffers on either queue, or an event on a decoder that subscribed to one.
	/// A caller that finds nothing ready afterwards just goes round again, so a
	/// spurious wakeup costs a loop rather than a wrong answer.
	///
	/// `revents` has to be read, not just the return value. `v4l2_m2m_poll`
	/// answers `POLLERR` whenever neither queue has a buffer queued, which is an
	/// ordinary transient here rather than a failure, and it is level triggered:
	/// a caller that took the immediate return for readiness would spin at full
	/// speed until its own deadline instead of parking. So the wait is finished
	/// by sleeping, which is what the caller asked for either way.
	pub(crate) fn wait(&self, timeout: Duration) {
		let mut event = libc::pollfd {
			fd: self.file.as_raw_fd(),
			events: libc::POLLIN | libc::POLLOUT | libc::POLLPRI,
			revents: 0,
		};
		// SAFETY: one initialized `pollfd` for the length passed.
		let ready = unsafe { libc::poll(&mut event, 1, timeout.as_millis().min(i32::MAX as u128) as libc::c_int) };
		// A negative return is `EINTR` or `ENOMEM` and leaves `revents` untouched,
		// so there is nothing to read and the caller's own deadline still bounds it.
		if ready > 0 && event.revents & (libc::POLLIN | libc::POLLOUT | libc::POLLPRI) == 0 {
			std::thread::sleep(timeout);
		}
	}
}

/// One mmap'd plane of one buffer.
struct Mapping {
	ptr: NonNull<u8>,
	len: usize,
}

// SAFETY: a mapping is only reached through `&Queue` / `&mut Queue`, and a queue
// travels with the backend that owns it, which the codec threads move as one
// value. Nothing here is shared between threads.
unsafe impl Send for Mapping {}

impl Drop for Mapping {
	fn drop(&mut self) {
		// SAFETY: the pointer and length are what `mmap` returned and this is the
		// only owner, so nothing can still be reading the region.
		let _ = unsafe { v4l::v4l2::munmap(self.ptr.as_ptr().cast(), self.len) };
	}
}

/// A queue's pool of mmap'd buffers, plus which of them are ours to fill.
pub(crate) struct Queue {
	dir: Dir,
	format: Format,
	buffers: Vec<Vec<Mapping>>,
	/// Buffers the driver is done with, so userspace may write them again.
	free: VecDeque<u32>,
	streaming: bool,
}

impl Queue {
	/// Allocate and map `count` buffers for `format` on `dir`.
	///
	/// Every buffer is zeroed once here rather than before each frame. Only the
	/// visible region is written afterwards, so the alignment padding a driver
	/// reads past the picture keeps the zeroes it started with, and a Raspberry
	/// Pi is not spending 3 MB of writes a frame to put them back.
	pub(crate) fn alloc(device: &Device, dir: Dir, format: Format, count: u32) -> Result<Self, Error> {
		let count = device.request_buffers(dir, count)?;
		if count == 0 {
			return Err(device.err("REQBUFS", std::io::Error::from(std::io::ErrorKind::OutOfMemory)));
		}

		let mut buffers = Vec::with_capacity(count as usize);
		for index in 0..count {
			let mut planes = zeroed_planes();
			let mut buffer = new_buffer(dir, format.planes.len());
			buffer.index = index;
			buffer.m.planes = planes.as_mut_ptr();

			// SAFETY: `VIDIOC_QUERYBUF` takes a `v4l2_buffer`, whose plane array is
			// the one above, valid for the call and long enough for `buffer.length`.
			unsafe { device.ioctl(vidioc::VIDIOC_QUERYBUF, &mut buffer) }
				.map_err(|err| device.err(format_args!("QUERYBUF {index}"), err))?;

			let mut mappings = Vec::with_capacity(format.planes.len());
			for plane in &planes[..format.planes.len()] {
				let len = plane.length as usize;
				// SAFETY: the offset is the cookie `QUERYBUF` just handed back for
				// this plane, on an mmap queue, so `m` holds `mem_offset`. The length
				// is the one it reported alongside.
				let ptr = unsafe {
					v4l::v4l2::mmap(
						std::ptr::null_mut(),
						len,
						libc::PROT_READ | libc::PROT_WRITE,
						libc::MAP_SHARED,
						device.file.as_raw_fd(),
						plane.m.mem_offset as libc::off_t,
					)
				}
				.map_err(|err| device.err(format_args!("mmap buffer {index}"), err))?;

				let ptr = NonNull::new(ptr.cast::<u8>())
					.ok_or_else(|| device.err("mmap", std::io::Error::from(std::io::ErrorKind::InvalidData)))?;
				// SAFETY: `mmap` just returned this region for `len` bytes and
				// nothing else has a pointer into it yet.
				unsafe { std::ptr::write_bytes(ptr.as_ptr(), 0, len) };
				mappings.push(Mapping { ptr, len });
			}
			buffers.push(mappings);
		}

		Ok(Self {
			dir,
			format,
			buffers,
			free: (0..count).collect(),
			streaming: false,
		})
	}

	/// The format this queue was allocated for.
	pub(crate) fn format(&self) -> &Format {
		&self.format
	}

	/// Take a buffer userspace may write, or `None` while the driver holds them
	/// all.
	pub(crate) fn take_free(&mut self) -> Option<u32> {
		self.free.pop_front()
	}

	/// Hand a dequeued buffer back to the free list.
	pub(crate) fn reclaim(&mut self, index: u32) {
		self.free.push_back(index);
	}

	/// One plane of one buffer, for the whole mapped length.
	pub(crate) fn plane(&self, index: u32, plane: usize) -> &[u8] {
		let mapping = &self.buffers[index as usize][plane];
		// SAFETY: the mapping is live for as long as this queue, and `&self`
		// excludes a concurrent write through `plane_mut`.
		unsafe { std::slice::from_raw_parts(mapping.ptr.as_ptr(), mapping.len) }
	}

	/// One plane of a dequeued buffer from where its payload starts, which a
	/// driver that puts a header on the plane reports through `data_offset`.
	pub(crate) fn payload(&self, buffer: &Dequeued, plane: usize) -> &[u8] {
		let mapping = self.plane(buffer.index, plane);
		&mapping[(buffer.data_offset[plane] as usize).min(mapping.len())..]
	}

	/// One plane of one buffer, writable.
	pub(crate) fn plane_mut(&mut self, index: u32, plane: usize) -> &mut [u8] {
		let mapping = &mut self.buffers[index as usize][plane];
		// SAFETY: as `plane`, and `&mut self` excludes any other view of it. The
		// driver does not touch a buffer that is not queued, and a buffer reaches
		// here only off the free list.
		unsafe { std::slice::from_raw_parts_mut(mapping.ptr.as_ptr(), mapping.len) }
	}

	/// Hand a buffer to the driver, `bytesused` bytes per plane, stamped with
	/// `timestamp`.
	///
	/// An M2M driver copies the OUTPUT timestamp onto the CAPTURE buffer its work
	/// comes out on, so this is how a frame's presentation time survives a codec
	/// that pipelines.
	pub(crate) fn queue(
		&self,
		device: &Device,
		index: u32,
		bytesused: &[u32],
		timestamp: Duration,
	) -> Result<(), Error> {
		let mut planes = zeroed_planes();
		// Driven by the buffer's own plane count, not by `bytesused`: every plane
		// the driver expects has to carry its mapped length, and a caller passing
		// one payload size for a several-plane picture (a decoder queueing an empty
		// buffer back) must not leave the rest reading as zero-length.
		for (index, mapping) in self.buffers[index as usize].iter().enumerate() {
			planes[index].bytesused = bytesused.get(index).copied().unwrap_or(0);
			planes[index].length = mapping.len as u32;
		}

		let mut buffer = new_buffer(self.dir, self.buffers[index as usize].len());
		buffer.index = index;
		buffer.m.planes = planes.as_mut_ptr();
		buffer.timestamp.tv_sec = timestamp.as_secs() as _;
		buffer.timestamp.tv_usec = timestamp.subsec_micros() as _;

		// SAFETY: `VIDIOC_QBUF` takes a `v4l2_buffer`, whose plane array is the one
		// above, valid for the call and long enough for `buffer.length`.
		unsafe { device.ioctl(vidioc::VIDIOC_QBUF, &mut buffer) }
			.map_err(|err| device.err(format_args!("QBUF {index}"), err))
	}

	/// Take back a buffer the driver has finished with.
	///
	/// The three answers are distinct because a drain needs them to be: "nothing
	/// ready yet" is a reason to wait and "the sequence has ended" is a reason to
	/// stop, and folding both into `None` is why a resolution change used to
	/// discard the pictures decoded before it.
	pub(crate) fn dequeue(&self, device: &Device) -> Result<Dequeue, Error> {
		// A queue that has not been started holds nothing, and asking anyway is an
		// error rather than the `EAGAIN` an empty one answers: `vb2` checks
		// `q->streaming` before it looks for a buffer. A caller that collects
		// finished buffers before it queues its first one would fail on that.
		if !self.streaming {
			return Ok(Dequeue::Empty);
		}

		let mut planes = zeroed_planes();
		let mut buffer = new_buffer(self.dir, self.format.planes.len());
		buffer.m.planes = planes.as_mut_ptr();

		// SAFETY: as `queue`, with `VIDIOC_DQBUF`.
		if let Err(err) = unsafe { device.ioctl(vidioc::VIDIOC_DQBUF, &mut buffer) } {
			return match err.kind() {
				// Nothing ready, which on a non-blocking fd is the answer rather than
				// a failure.
				std::io::ErrorKind::WouldBlock => Ok(Dequeue::Empty),
				// `EPIPE`, which is what the kernel answers a dequeue past the buffer
				// flagged `V4L2_BUF_FLAG_LAST` with. Not an error: it says the drain
				// this caller is driving has already finished.
				std::io::ErrorKind::BrokenPipe => Ok(Dequeue::Ended),
				_ => Err(device.err("DQBUF", err)),
			};
		}

		let mut bytesused = [0; MAX_PLANES];
		let mut data_offset = [0; MAX_PLANES];
		for (index, plane) in planes.iter().enumerate() {
			bytesused[index] = plane.bytesused;
			data_offset[index] = plane.data_offset;
		}

		Ok(Dequeue::Buffer(Dequeued {
			index: buffer.index,
			bytesused,
			data_offset,
			timestamp: timestamp(buffer.timestamp),
			flags: buffer.flags,
		}))
	}

	/// Start the queue, which the driver only accepts once its buffers are
	/// allocated.
	pub(crate) fn stream_on(&mut self, device: &Device) -> Result<(), Error> {
		device.stream(self.dir, true)?;
		self.streaming = true;
		Ok(())
	}

	/// Stop and restart a queue, taking every buffer back from the driver.
	pub(crate) fn restart(&mut self, device: &Device) -> Result<(), Error> {
		if self.streaming {
			device.stream(self.dir, false)?;
			self.streaming = false;
		}
		self.free = (0..self.buffers.len() as u32).collect();
		self.stream_on(device)
	}

	/// Whether the queue has been started.
	pub(crate) fn streaming(&self) -> bool {
		self.streaming
	}

	/// Buffers currently owned by the driver.
	pub(crate) fn outstanding(&self) -> usize {
		self.buffers.len() - self.free.len()
	}

	/// Stop the queue and hand its buffers back to the driver, which is how a
	/// decoder re-negotiates CAPTURE when the stream changes size.
	///
	/// Consumes the queue: the buffers it addressed no longer exist afterwards,
	/// so there is nothing left that could be queued by mistake.
	pub(crate) fn release(mut self, device: &Device) -> Result<(), Error> {
		if self.streaming {
			device.stream(self.dir, false)?;
			self.streaming = false;
		}
		// Unmapped before the driver is asked to free them: `REQBUFS(0)` returns
		// EBUSY while userspace still holds a mapping.
		self.buffers.clear();
		self.free.clear();
		device.request_buffers(self.dir, 0).map(|_| ())
	}
}

/// What a `VIDIOC_DQBUF` found.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Dequeue {
	/// A buffer the driver has finished with.
	Buffer(Dequeued),
	/// Nothing is ready yet, so a caller with time left should wait.
	Empty,
	/// The sequence has already ended, so there is nothing left to wait for.
	Ended,
}

impl Dequeue {
	/// The buffer, if there was one.
	///
	/// Folds `Ended` back into "nothing more", which is what a caller that is not
	/// driving a drain wants: it collects what is ready and comes back later.
	pub(crate) fn buffer(self) -> Option<Dequeued> {
		match self {
			Dequeue::Buffer(buffer) => Some(buffer),
			Dequeue::Empty | Dequeue::Ended => None,
		}
	}
}

/// A buffer the driver has handed back.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Dequeued {
	/// Which buffer of the pool it is.
	pub index: u32,
	/// Bytes the driver wrote, per plane, counted from the start of the plane
	/// and so including any header ahead of the payload. Zero on an OUTPUT
	/// buffer, which the driver only read.
	pub bytesused: [u32; MAX_PLANES],
	/// Where the payload starts in each plane. Zero from every codec driver
	/// seen so far, but the UAPI lets a driver put a header ahead of the data
	/// and say so here.
	data_offset: [u32; MAX_PLANES],
	/// The timestamp the matching OUTPUT buffer carried, copied through by the
	/// driver.
	pub timestamp: Duration,
	/// The `V4L2_BUF_FLAG_*` bits the driver set, read through [`Dequeued::failed`]
	/// and [`Dequeued::last`].
	flags: u32,
}

impl Dequeued {
	/// Bytes of payload in `plane`, past whatever header the driver put ahead
	/// of it.
	pub(crate) fn written(&self, plane: usize) -> u32 {
		self.bytesused[plane].saturating_sub(self.data_offset[plane])
	}

	/// Whether the driver marked the buffer's contents unusable.
	///
	/// `VIDIOC_DQBUF` succeeds for a buffer flagged `V4L2_BUF_FLAG_ERROR`, so the
	/// flag is the only thing that separates a good buffer from a bad one: an
	/// encoder that overran its coded buffer returns a truncated access unit this
	/// way, and a decoder that failed a picture returns garbage.
	pub(crate) fn failed(&self) -> bool {
		self.flags & V4L2_BUF_FLAG_ERROR != 0
	}

	/// Whether this is the last buffer of a sequence.
	///
	/// The kernel ends every drain with it, whether the drain was asked for with
	/// an encoder or decoder command or implied by a resolution change. The buffer
	/// may still be empty, in which case it marks the end and is not a frame.
	pub(crate) fn last(&self) -> bool {
		self.flags & V4L2_BUF_FLAG_LAST != 0
	}
}

/// The timestamp a dequeued buffer carries.
///
/// Both fields of a `struct timeval` are signed and 64 bits wide, and neither
/// is worth trusting into anything narrower: a driver that leaves a buffer's
/// timestamp alone hands back whatever the struct held, and
/// `tv_usec as u32 * 1_000` overflows for any `tv_usec` above 4_294_967, which
/// panics in a debug build. Negative values are not a time either, so they
/// clamp to zero rather than wrapping into the far future.
fn timestamp(time: timeval) -> Duration {
	Duration::from_secs(time.tv_sec.max(0) as u64) + Duration::from_micros(time.tv_usec.max(0) as u64)
}

/// A zeroed plane array for a `v4l2_buffer` to point at.
fn zeroed_planes() -> [v4l2_plane; MAX_PLANES] {
	<[v4l2_plane; MAX_PLANES]>::zeroed()
}

/// A zeroed `v4l2_buffer` addressing `planes` planes of an mmap queue.
fn new_buffer(dir: Dir, planes: usize) -> v4l2_buffer {
	let mut buffer = v4l2_buffer::zeroed();
	buffer.type_ = dir.buf_type();
	buffer.memory = v4l2_memory_V4L2_MEMORY_MMAP;
	// On a multi-planar queue this field is the length of the plane array, not a
	// byte count.
	buffer.length = planes as u32;
	buffer
}

/// A rectangle inside a coded picture: the part of it that is the picture.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Rect {
	/// Pixels from the left edge of the coded picture.
	pub left: u32,
	/// Rows from the top of the coded picture.
	pub top: u32,
	pub size: Size,
}

impl Rect {
	/// The whole of a `size` picture, from its top-left corner.
	pub(crate) fn whole(size: Size) -> Self {
		Self { left: 0, top: 0, size }
	}
}

/// Where a raw 4:2:0 frame's samples sit inside a queue's buffers.
///
/// The whole point of the type: a driver answers `VIDIOC_S_FMT` with its own
/// fourcc, its own stride, and its own buffer size, and none of the three has to
/// be what was asked for. `bcm2835-codec` aligns stride to 32 bytes and height
/// to 16 rows, and reports the row padding through `bytesperline` but the row
/// *count* only through `sizeimage`. Chroma therefore starts at
/// `stride * padded_rows`, not at `stride * height`: place it at the visible
/// height and the driver reads zeroes for chroma, which comes out as a green
/// picture.
pub(crate) struct Planes {
	y: Component,
	/// Interleaved chroma for a semi-planar format, or the U plane of a planar
	/// one.
	u: Component,
	/// The V plane, absent when chroma is interleaved.
	v: Option<Component>,
	/// The visible size, which is what gets copied. It fits inside the format's
	/// coded size, at the origin the components' offsets already account for.
	size: Size,
}

/// One component's position within the queue's buffers.
#[derive(Clone, Copy)]
struct Component {
	plane: usize,
	offset: usize,
	stride: usize,
}

impl Planes {
	/// Work out where the `rect` part of a coded picture sits in `format`'s
	/// buffers.
	pub(crate) fn new(format: &Format, rect: Rect) -> Result<Self, Error> {
		let size = rect.size;
		size.validate("V4L2 4:2:0 frame")?;
		if rect.left + size.width > format.size.width || rect.top + size.height > format.size.height {
			return Err(Error::Codec(anyhow::anyhow!(
				"V4L2 negotiated {} for a {size} picture at {},{}",
				format.size,
				rect.left,
				rect.top
			)));
		}
		// Chroma is subsampled by two in each direction, and H.264 crops in units
		// of two for exactly that reason, so halving loses nothing.
		let (left, top) = (rect.left as usize, rect.top as usize);
		let (chroma_left, chroma_top) = (left / 2, top / 2);

		let interleaved = match format.pixelformat {
			NV12 | NV12M => true,
			YUV420 | YUV420M => false,
			other => {
				return Err(Error::Codec(anyhow::anyhow!(
					"V4L2 chose the unsupported raw format {}",
					name(other)
				)));
			}
		};

		let luma = *format
			.planes
			.first()
			.ok_or_else(|| Error::Codec(anyhow::anyhow!("V4L2 reported a format with no planes")))?;
		let stride = luma.stride.max(format.size.width) as usize;
		let y = Component {
			plane: 0,
			offset: top * stride + left,
			stride,
		};

		// Keyed on the plane count rather than the fourcc: the contiguous and
		// per-plane spellings of a layout differ only in where the planes land, and
		// a driver is free to answer either request with the other.
		let separate = format.planes.len() > 1;
		let rows = padded_rows(luma, format.size.height);

		let (u, v) = match (interleaved, separate) {
			(true, true) => {
				let stride = format.planes[1].stride.max(format.size.width) as usize;
				(
					Component {
						plane: 1,
						// Interleaved chroma is two bytes per sample pair, so a pixel
						// column is a byte column.
						offset: chroma_top * stride + left,
						stride,
					},
					None,
				)
			}
			(true, false) => (
				Component {
					plane: 0,
					offset: stride * rows + chroma_top * stride + left,
					stride,
				},
				None,
			),
			(false, true) if format.planes.len() >= 3 => {
				let u_stride = format.planes[1].stride.max(format.size.width / 2) as usize;
				let v_stride = format.planes[2].stride.max(format.size.width / 2) as usize;
				(
					Component {
						plane: 1,
						offset: chroma_top * u_stride + chroma_left,
						stride: u_stride,
					},
					Some(Component {
						plane: 2,
						offset: chroma_top * v_stride + chroma_left,
						stride: v_stride,
					}),
				)
			}
			(false, true) => {
				return Err(Error::Codec(anyhow::anyhow!(
					"V4L2 chose planar {} with {} planes",
					name(format.pixelformat),
					format.planes.len()
				)));
			}
			(false, false) => {
				let chroma_stride = stride / 2;
				let chroma_origin = chroma_top * chroma_stride + chroma_left;
				(
					Component {
						plane: 0,
						offset: stride * rows + chroma_origin,
						stride: chroma_stride,
					},
					Some(Component {
						plane: 0,
						offset: stride * rows + chroma_stride * rows.div_ceil(2) + chroma_origin,
						stride: chroma_stride,
					}),
				)
			}
		};

		Ok(Self { y, u, v, size })
	}

	/// Copy a frame into buffer `index`, laying each plane out the way the driver
	/// asked for.
	pub(crate) fn write(&self, queue: &mut Queue, index: u32, frame: &I420) -> Result<(), Error> {
		let (width, height) = (self.size.width as usize, self.size.height as usize);
		let (chroma_width, chroma_rows) = (width / 2, height / 2);

		scatter(queue.plane_mut(index, self.y.plane), self.y, frame.y(), width, height)?;
		match self.v {
			Some(v) => {
				scatter(
					queue.plane_mut(index, self.u.plane),
					self.u,
					frame.u(),
					chroma_width,
					chroma_rows,
				)?;
				scatter(queue.plane_mut(index, v.plane), v, frame.v(), chroma_width, chroma_rows)?;
			}
			None => interleave(
				queue.plane_mut(index, self.u.plane),
				self.u,
				frame.u(),
				frame.v(),
				chroma_width,
				chroma_rows,
			)?,
		}
		Ok(())
	}

	/// Copy a picture out of a dequeued buffer, undoing the driver's stride and
	/// chroma layout.
	pub(crate) fn read(&self, queue: &Queue, buffer: &Dequeued) -> Result<I420, Error> {
		let (width, height) = (self.size.width as usize, self.size.height as usize);
		let (chroma_width, chroma_rows) = (width / 2, height / 2);

		let mut data = vec![0u8; I420::len(self.size.width, self.size.height)];
		let (luma, chroma) = data.split_at_mut(width * height);
		let (u, v) = chroma.split_at_mut(chroma_width * chroma_rows);

		gather(luma, queue.payload(buffer, self.y.plane), self.y, width, height)?;
		match self.v {
			Some(at) => {
				gather(
					u,
					queue.payload(buffer, self.u.plane),
					self.u,
					chroma_width,
					chroma_rows,
				)?;
				gather(v, queue.payload(buffer, at.plane), at, chroma_width, chroma_rows)?;
			}
			None => deinterleave(
				u,
				v,
				queue.payload(buffer, self.u.plane),
				self.u,
				chroma_width,
				chroma_rows,
			)?,
		}

		I420::new(self.size.width, self.size.height, data)
	}
}

/// Luma rows the driver reserves before chroma starts.
///
/// A driver that pads the height reports the padding only through `sizeimage`,
/// so a contiguous 4:2:0 buffer's chroma offset has to be recovered from it. The
/// floor at the visible height keeps a driver whose `sizeimage` carries an
/// unrelated tail from placing chroma inside the picture.
fn padded_rows(luma: Plane, height: u32) -> usize {
	let height = height as usize;
	match luma.stride as usize {
		// A driver that reports no stride is reporting no padding either.
		0 => height,
		// One buffer holding 4:2:0: sizeimage = stride * rows * 3 / 2.
		stride => (luma.sizeimage as usize * 2 / (stride * 3)).max(height),
	}
}

/// Copy `rows` rows of `width` tightly packed bytes into a strided plane.
fn scatter(dst: &mut [u8], at: Component, src: &[u8], width: usize, rows: usize) -> Result<(), Error> {
	let len = dst.len();
	for row in 0..rows {
		let start = at.offset + row * at.stride;
		dst.get_mut(start..start + width)
			.ok_or_else(|| short(len, start + width))?
			.copy_from_slice(&src[row * width..][..width]);
	}
	Ok(())
}

/// Interleave tightly packed U and V rows into a strided semi-planar chroma
/// plane, `width` sample pairs per row.
fn interleave(dst: &mut [u8], at: Component, u: &[u8], v: &[u8], width: usize, rows: usize) -> Result<(), Error> {
	let len = dst.len();
	for row in 0..rows {
		let start = at.offset + row * at.stride;
		let out = dst
			.get_mut(start..start + width * 2)
			.ok_or_else(|| short(len, start + width * 2))?;
		let (u, v) = (&u[row * width..][..width], &v[row * width..][..width]);
		for (pair, (u, v)) in out.chunks_exact_mut(2).zip(u.iter().zip(v)) {
			pair[0] = *u;
			pair[1] = *v;
		}
	}
	Ok(())
}

/// Copy `rows` rows of `width` bytes out of a strided plane, tightly packed.
fn gather(dst: &mut [u8], src: &[u8], at: Component, width: usize, rows: usize) -> Result<(), Error> {
	for row in 0..rows {
		let start = at.offset + row * at.stride;
		let line = src
			.get(start..start + width)
			.ok_or_else(|| short(src.len(), start + width))?;
		dst[row * width..][..width].copy_from_slice(line);
	}
	Ok(())
}

/// Split a strided semi-planar chroma plane into tightly packed U and V,
/// `width` sample pairs per row.
fn deinterleave(u: &mut [u8], v: &mut [u8], src: &[u8], at: Component, width: usize, rows: usize) -> Result<(), Error> {
	for row in 0..rows {
		let start = at.offset + row * at.stride;
		let line = src
			.get(start..start + width * 2)
			.ok_or_else(|| short(src.len(), start + width * 2))?;
		let (u, v) = (&mut u[row * width..][..width], &mut v[row * width..][..width]);
		for (pair, (u, v)) in line.chunks_exact(2).zip(u.iter_mut().zip(v)) {
			*u = pair[0];
			*v = pair[1];
		}
	}
	Ok(())
}

fn short(len: usize, needed: usize) -> Error {
	Error::Codec(anyhow::anyhow!(
		"V4L2 buffer of {len} bytes is too small for the {needed} its format implies"
	))
}

#[cfg(test)]
mod tests {
	use super::*;

	/// A single-buffer format the way a driver reports one.
	fn format(pixelformat: u32, size: Size, stride: u32, rows: u32) -> Format {
		Format {
			pixelformat,
			size,
			planes: vec![Plane {
				stride,
				sizeimage: stride * rows * 3 / 2,
			}],
		}
	}

	/// A `tv_usec` past a `u32` of nanoseconds used to panic in debug rather than
	/// convert, and a driver that never wrote the field is free to hand back
	/// anything at all.
	#[test]
	fn a_buffer_timestamp_survives_any_timeval() {
		assert_eq!(
			timestamp(timeval {
				tv_sec: 12,
				tv_usec: 345_678,
			}),
			Duration::from_micros(12_345_678)
		);
		// Above `u32::MAX / 1_000` microseconds, which is what overflowed.
		assert_eq!(
			timestamp(timeval {
				tv_sec: 0,
				tv_usec: 5_000_000,
			}),
			Duration::from_secs(5)
		);
		assert_eq!(
			timestamp(timeval {
				tv_sec: -1,
				tv_usec: -1,
			}),
			Duration::ZERO
		);
	}

	/// `bytesused` counts from the start of the plane, so the payload is what is
	/// left past the header the driver reported, and a header past the count is
	/// no payload at all.
	#[test]
	fn the_payload_is_past_the_data_offset() {
		let buffer = Dequeued {
			index: 0,
			bytesused: [10, 0, 0],
			data_offset: [4, 0, 0],
			timestamp: Duration::ZERO,
			flags: 0,
		};
		assert_eq!(buffer.written(0), 6);
		let buffer = Dequeued {
			data_offset: [16, 0, 0],
			..buffer
		};
		assert_eq!(buffer.written(0), 0);
	}

	/// The bug the alignment work was about: chroma goes where the padded row
	/// count puts it, not where the visible height would.
	#[test]
	fn chroma_follows_the_padded_height() {
		// 640x360 padded to a 640-byte stride and 368 rows, which is what a
		// 16-row-aligned driver reports.
		let planes = Planes::new(
			&format(NV12, Size::new(640, 368), 640, 368),
			Rect::whole(Size::new(640, 360)),
		)
		.unwrap();
		assert_eq!(planes.u.offset, 640 * 368);
		assert!(planes.v.is_none());
	}

	/// A driver that pads the stride but not the height still has to be believed
	/// about the stride.
	#[test]
	fn chroma_follows_the_padded_stride() {
		let planes = Planes::new(
			&format(YUV420, Size::new(360, 240), 384, 240),
			Rect::whole(Size::new(360, 240)),
		)
		.unwrap();
		assert_eq!(planes.y.stride, 384);
		assert_eq!(planes.u.offset, 384 * 240);
		assert_eq!(planes.u.stride, 192);
		let v = planes.v.unwrap();
		assert_eq!(v.offset, 384 * 240 + 192 * 120);
		assert_eq!(v.stride, 192);
	}

	/// A compose rectangle that does not start at the corner moves every
	/// component's origin, chroma by half as much in each direction.
	#[test]
	fn the_picture_starts_at_the_compose_origin() {
		let rect = Rect {
			left: 16,
			top: 8,
			size: Size::new(1888, 1072),
		};
		let planes = Planes::new(&format(NV12, Size::new(1920, 1088), 1920, 1088), rect).unwrap();
		assert_eq!(planes.y.offset, 8 * 1920 + 16);
		assert_eq!(planes.u.offset, 1920 * 1088 + 4 * 1920 + 16);

		let planes = Planes::new(&format(YUV420, Size::new(1920, 1088), 1920, 1088), rect).unwrap();
		assert_eq!(planes.y.offset, 8 * 1920 + 16);
		assert_eq!(planes.u.offset, 1920 * 1088 + 4 * 960 + 8);
		assert_eq!(planes.v.unwrap().offset, 1920 * 1088 + 960 * 544 + 4 * 960 + 8);
	}

	/// Per-plane formats put each component in its own buffer at offset zero.
	#[test]
	fn separate_planes_start_at_zero() {
		let format = Format {
			pixelformat: NV12M,
			size: Size::new(320, 240),
			planes: vec![
				Plane {
					stride: 320,
					sizeimage: 320 * 240,
				},
				Plane {
					stride: 320,
					sizeimage: 320 * 120,
				},
			],
		};
		let planes = Planes::new(&format, Rect::whole(Size::new(320, 240))).unwrap();
		assert_eq!(planes.u.plane, 1);
		assert_eq!(planes.u.offset, 0);
	}

	/// A picture larger than what the driver negotiated would be written past the
	/// end of the buffer, so it is refused rather than truncated.
	#[test]
	fn a_picture_larger_than_the_format_is_refused() {
		let format = format(NV12, Size::new(320, 240), 320, 240);
		assert!(Planes::new(&format, Rect::whole(Size::new(640, 480))).is_err());
		// So is a picture that fits only if its origin is ignored.
		assert!(
			Planes::new(
				&format,
				Rect {
					left: 16,
					top: 0,
					size: Size::new(320, 240)
				}
			)
			.is_err()
		);
	}

	/// A driver offering something we cannot lay out says so at open, not on the
	/// first frame.
	#[test]
	fn an_unsupported_raw_format_is_refused() {
		let format = format(fourcc(*b"RGB3"), Size::new(320, 240), 960, 240);
		assert!(Planes::new(&format, Rect::whole(Size::new(320, 240))).is_err());
	}

	/// The written picture round-trips: every row lands at the driver's stride
	/// and chroma is interleaved in the driver's order.
	#[test]
	fn writing_respects_the_stride() {
		let width = 4;
		let height = 4;
		let stride = 8;
		let at = Component {
			plane: 0,
			offset: 0,
			stride,
		};

		let mut dst = vec![0u8; stride * height];
		let src: Vec<u8> = (0..(width * height) as u8).collect();
		scatter(&mut dst, at, &src, width, height).unwrap();
		assert_eq!(&dst[..width], &src[..width]);
		assert_eq!(&dst[stride..stride + width], &src[width..width * 2]);
		// The padding between rows is left alone.
		assert_eq!(&dst[width..stride], &[0; 4]);

		let mut chroma = vec![0u8; stride * height];
		interleave(&mut chroma, at, &[1, 2], &[3, 4], 2, 1).unwrap();
		assert_eq!(&chroma[..4], &[1, 3, 2, 4]);
	}

	/// Reading undoes writing, for both chroma layouts. The two directions are
	/// separate code, so a stride or an interleave order that disagreed between
	/// them would decode every frame with its colors swapped.
	#[test]
	fn reading_undoes_writing() {
		let (width, rows, stride) = (4, 4, 8);
		let at = Component {
			plane: 0,
			offset: 16,
			stride,
		};

		let luma: Vec<u8> = (0..(width * rows) as u8).collect();
		let mut device = vec![0u8; at.offset + stride * rows];
		scatter(&mut device, at, &luma, width, rows).unwrap();
		let mut back = vec![0u8; width * rows];
		gather(&mut back, &device, at, width, rows).unwrap();
		assert_eq!(back, luma);

		let (u, v) = (vec![1, 2, 3, 4], vec![5, 6, 7, 8]);
		let mut device = vec![0u8; at.offset + stride * rows];
		interleave(&mut device, at, &u, &v, 2, 2).unwrap();
		let (mut back_u, mut back_v) = (vec![0u8; 4], vec![0u8; 4]);
		deinterleave(&mut back_u, &mut back_v, &device, at, 2, 2).unwrap();
		assert_eq!(back_u, u);
		assert_eq!(back_v, v);
	}

	/// A buffer smaller than the format implies is an error, not a panic in the
	/// middle of a frame.
	#[test]
	fn a_short_buffer_errors() {
		let at = Component {
			plane: 0,
			offset: 0,
			stride: 8,
		};
		let mut dst = vec![0u8; 8];
		assert!(scatter(&mut dst, at, &[0; 16], 4, 4).is_err());

		let mut back = vec![0u8; 16];
		assert!(gather(&mut back, &[0; 8], at, 4, 4).is_err());
	}
}
