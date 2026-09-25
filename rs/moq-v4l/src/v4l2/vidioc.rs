use crate::sys::*;

#[cfg(not(target_env = "musl"))]
#[allow(non_camel_case_types)]
pub type _IOC_TYPE = std::os::raw::c_ulong;
#[cfg(target_env = "musl")]
#[allow(non_camel_case_types)]
pub type _IOC_TYPE = std::os::raw::c_int;

// linux ioctl.h
const _IOC_NRBITS: u8 = 8;
const _IOC_TYPEBITS: u8 = 8;

const _IOC_SIZEBITS: u8 = 14;

const _IOC_NRSHIFT: u8 = 0;
const _IOC_TYPESHIFT: u8 = _IOC_NRSHIFT + _IOC_NRBITS;
const _IOC_SIZESHIFT: u8 = _IOC_TYPESHIFT + _IOC_TYPEBITS;
const _IOC_DIRSHIFT: u8 = _IOC_SIZESHIFT + _IOC_SIZEBITS;

const _IOC_NONE: u8 = 0;
const _IOC_WRITE: u8 = 1;
const _IOC_READ: u8 = 2;

macro_rules! _IOC_TYPECHECK {
	($type:ty) => {
		std::mem::size_of::<$type>()
	};
}

macro_rules! _IOC {
	($dir:expr, $type:expr, $nr:expr, $size:expr) => {
		(($dir as _IOC_TYPE) << $crate::v4l2::vidioc::_IOC_DIRSHIFT)
			| (($type as _IOC_TYPE) << $crate::v4l2::vidioc::_IOC_TYPESHIFT)
			| (($nr as _IOC_TYPE) << $crate::v4l2::vidioc::_IOC_NRSHIFT)
			| (($size as _IOC_TYPE) << $crate::v4l2::vidioc::_IOC_SIZESHIFT)
	};
}

macro_rules! _IO {
	($type:expr, $nr:expr) => {
		_IOC!($crate::v4l2::vidioc::_IOC_NONE, $type, $nr, 0)
	};
}

macro_rules! _IOR {
	($type:expr, $nr:expr, $size:ty) => {
		_IOC!($crate::v4l2::vidioc::_IOC_READ, $type, $nr, _IOC_TYPECHECK!($size))
	};
}

macro_rules! _IOW {
	($type:expr, $nr:expr, $size:ty) => {
		_IOC!($crate::v4l2::vidioc::_IOC_WRITE, $type, $nr, _IOC_TYPECHECK!($size))
	};
}

macro_rules! _IOWR {
	($type:expr, $nr:expr, $size:ty) => {
		_IOC!(
			$crate::v4l2::vidioc::_IOC_READ | $crate::v4l2::vidioc::_IOC_WRITE,
			$type,
			$nr,
			_IOC_TYPECHECK!($size)
		)
	};
}

pub const VIDIOC_QUERYCAP: _IOC_TYPE = _IOR!(b'V', 0, v4l2_capability);
pub const VIDIOC_RESERVED: _IOC_TYPE = _IO!(b'V', 1);
pub const VIDIOC_ENUM_FMT: _IOC_TYPE = _IOWR!(b'V', 2, v4l2_fmtdesc);
pub const VIDIOC_G_FMT: _IOC_TYPE = _IOWR!(b'V', 4, v4l2_format);
pub const VIDIOC_S_FMT: _IOC_TYPE = _IOWR!(b'V', 5, v4l2_format);
pub const VIDIOC_REQBUFS: _IOC_TYPE = _IOWR!(b'V', 8, v4l2_requestbuffers);
pub const VIDIOC_QUERYBUF: _IOC_TYPE = _IOWR!(b'V', 9, v4l2_buffer);
pub const VIDIOC_G_FBUF: _IOC_TYPE = _IOR!(b'V', 10, v4l2_framebuffer);
pub const VIDIOC_S_FBUF: _IOC_TYPE = _IOW!(b'V', 11, v4l2_framebuffer);
pub const VIDIOC_OVERLAY: _IOC_TYPE = _IOW!(b'V', 14, std::os::raw::c_int);
pub const VIDIOC_QBUF: _IOC_TYPE = _IOWR!(b'V', 15, v4l2_buffer);
pub const VIDIOC_EXPBUF: _IOC_TYPE = _IOWR!(b'V', 16, v4l2_exportbuffer);
pub const VIDIOC_DQBUF: _IOC_TYPE = _IOWR!(b'V', 17, v4l2_buffer);
pub const VIDIOC_STREAMON: _IOC_TYPE = _IOW!(b'V', 18, std::os::raw::c_int);
pub const VIDIOC_STREAMOFF: _IOC_TYPE = _IOW!(b'V', 19, std::os::raw::c_int);
pub const VIDIOC_G_PARM: _IOC_TYPE = _IOWR!(b'V', 21, v4l2_streamparm);
pub const VIDIOC_S_PARM: _IOC_TYPE = _IOWR!(b'V', 22, v4l2_streamparm);
pub const VIDIOC_G_STD: _IOC_TYPE = _IOR!(b'V', 23, v4l2_std_id);
pub const VIDIOC_S_STD: _IOC_TYPE = _IOW!(b'V', 24, v4l2_std_id);
pub const VIDIOC_ENUMSTD: _IOC_TYPE = _IOWR!(b'V', 25, v4l2_standard);
pub const VIDIOC_ENUMINPUT: _IOC_TYPE = _IOWR!(b'V', 26, v4l2_input);
pub const VIDIOC_G_CTRL: _IOC_TYPE = _IOWR!(b'V', 27, v4l2_control);
pub const VIDIOC_S_CTRL: _IOC_TYPE = _IOWR!(b'V', 28, v4l2_control);
pub const VIDIOC_G_TUNER: _IOC_TYPE = _IOWR!(b'V', 29, v4l2_tuner);
pub const VIDIOC_S_TUNER: _IOC_TYPE = _IOW!(b'V', 30, v4l2_tuner);
pub const VIDIOC_G_AUDIO: _IOC_TYPE = _IOR!(b'V', 33, v4l2_audio);
pub const VIDIOC_S_AUDIO: _IOC_TYPE = _IOW!(b'V', 34, v4l2_audio);
pub const VIDIOC_QUERYCTRL: _IOC_TYPE = _IOWR!(b'V', 36, v4l2_queryctrl);
pub const VIDIOC_QUERYMENU: _IOC_TYPE = _IOWR!(b'V', 37, v4l2_querymenu);
pub const VIDIOC_G_INPUT: _IOC_TYPE = _IOR!(b'V', 38, std::os::raw::c_int);
pub const VIDIOC_S_INPUT: _IOC_TYPE = _IOWR!(b'V', 39, std::os::raw::c_int);
pub const VIDIOC_G_EDID: _IOC_TYPE = _IOWR!(b'V', 40, v4l2_edid);
pub const VIDIOC_S_EDID: _IOC_TYPE = _IOWR!(b'V', 41, v4l2_edid);
pub const VIDIOC_G_OUTPUT: _IOC_TYPE = _IOR!(b'V', 46, std::os::raw::c_int);
pub const VIDIOC_S_OUTPUT: _IOC_TYPE = _IOWR!(b'V', 47, std::os::raw::c_int);
pub const VIDIOC_ENUMOUTPUT: _IOC_TYPE = _IOWR!(b'V', 48, v4l2_output);
pub const VIDIOC_G_AUDOUT: _IOC_TYPE = _IOR!(b'V', 49, v4l2_audioout);
pub const VIDIOC_S_AUDOUT: _IOC_TYPE = _IOW!(b'V', 50, v4l2_audioout);
pub const VIDIOC_G_MODULATOR: _IOC_TYPE = _IOWR!(b'V', 54, v4l2_modulator);
pub const VIDIOC_S_MODULATOR: _IOC_TYPE = _IOW!(b'V', 55, v4l2_modulator);
pub const VIDIOC_G_FREQUENCY: _IOC_TYPE = _IOWR!(b'V', 56, v4l2_frequency);
pub const VIDIOC_S_FREQUENCY: _IOC_TYPE = _IOW!(b'V', 57, v4l2_frequency);
pub const VIDIOC_CROPCAP: _IOC_TYPE = _IOWR!(b'V', 58, v4l2_cropcap);
pub const VIDIOC_G_CROP: _IOC_TYPE = _IOWR!(b'V', 59, v4l2_crop);
pub const VIDIOC_S_CROP: _IOC_TYPE = _IOW!(b'V', 60, v4l2_crop);
pub const VIDIOC_G_JPEGCOMP: _IOC_TYPE = _IOR!(b'V', 61, v4l2_jpegcompression);
pub const VIDIOC_S_JPEGCOMP: _IOC_TYPE = _IOW!(b'V', 62, v4l2_jpegcompression);
pub const VIDIOC_QUERYSTD: _IOC_TYPE = _IOR!(b'V', 63, v4l2_std_id);
pub const VIDIOC_TRY_FMT: _IOC_TYPE = _IOWR!(b'V', 64, v4l2_format);
pub const VIDIOC_ENUMAUDIO: _IOC_TYPE = _IOWR!(b'V', 65, v4l2_audio);
pub const VIDIOC_ENUMAUDOUT: _IOC_TYPE = _IOWR!(b'V', 66, v4l2_audioout);
pub const VIDIOC_G_PRIORITY: _IOC_TYPE = _IOR!(b'V', 67, std::os::raw::c_int);
pub const VIDIOC_S_PRIORITY: _IOC_TYPE = _IOW!(b'V', 68, std::os::raw::c_int);
pub const VIDIOC_G_SLICED_VBI_CAP: _IOC_TYPE = _IOWR!(b'V', 69, v4l2_sliced_vbi_cap);
pub const VIDIOC_LOG_STATUS: _IOC_TYPE = _IO!(b'V', 70);
pub const VIDIOC_G_EXT_CTRLS: _IOC_TYPE = _IOWR!(b'V', 71, v4l2_ext_controls);
pub const VIDIOC_S_EXT_CTRLS: _IOC_TYPE = _IOWR!(b'V', 72, v4l2_ext_controls);
pub const VIDIOC_TRY_EXT_CTRLS: _IOC_TYPE = _IOWR!(b'V', 73, v4l2_ext_controls);
pub const VIDIOC_ENUM_FRAMESIZES: _IOC_TYPE = _IOWR!(b'V', 74, v4l2_frmsizeenum);
pub const VIDIOC_ENUM_FRAMEINTERVALS: _IOC_TYPE = _IOWR!(b'V', 75, v4l2_frmivalenum);
pub const VIDIOC_G_ENC_INDEX: _IOC_TYPE = _IOR!(b'V', 76, v4l2_enc_idx);
pub const VIDIOC_ENCODER_CMD: _IOC_TYPE = _IOWR!(b'V', 77, v4l2_encoder_cmd);
pub const VIDIOC_TRY_ENCODER_CMD: _IOC_TYPE = _IOWR!(b'V', 78, v4l2_encoder_cmd);
pub const VIDIOC_DBG_S_REGISTER: _IOC_TYPE = _IOW!(b'V', 79, v4l2_dbg_register);
pub const VIDIOC_DBG_G_REGISTER: _IOC_TYPE = _IOWR!(b'V', 80, v4l2_dbg_register);
pub const VIDIOC_S_HW_FREQ_SEEK: _IOC_TYPE = _IOW!(b'V', 82, v4l2_hw_freq_seek);
pub const VIDIOC_S_DV_TIMINGS: _IOC_TYPE = _IOWR!(b'V', 87, v4l2_dv_timings);
pub const VIDIOC_G_DV_TIMINGS: _IOC_TYPE = _IOWR!(b'V', 88, v4l2_dv_timings);
pub const VIDIOC_DQEVENT: _IOC_TYPE = _IOR!(b'V', 89, v4l2_event);
pub const VIDIOC_SUBSCRIBE_EVENT: _IOC_TYPE = _IOW!(b'V', 90, v4l2_event_subscription);
pub const VIDIOC_UNSUBSCRIBE_EVENT: _IOC_TYPE = _IOW!(b'V', 91, v4l2_event_subscription);
pub const VIDIOC_CREATE_BUFS: _IOC_TYPE = _IOWR!(b'V', 92, v4l2_create_buffers);
pub const VIDIOC_PREPARE_BUF: _IOC_TYPE = _IOWR!(b'V', 93, v4l2_buffer);
pub const VIDIOC_G_SELECTION: _IOC_TYPE = _IOWR!(b'V', 94, v4l2_selection);
pub const VIDIOC_S_SELECTION: _IOC_TYPE = _IOWR!(b'V', 95, v4l2_selection);
pub const VIDIOC_DECODER_CMD: _IOC_TYPE = _IOWR!(b'V', 96, v4l2_decoder_cmd);
pub const VIDIOC_TRY_DECODER_CMD: _IOC_TYPE = _IOWR!(b'V', 97, v4l2_decoder_cmd);
pub const VIDIOC_ENUM_DV_TIMINGS: _IOC_TYPE = _IOWR!(b'V', 98, v4l2_enum_dv_timings);
pub const VIDIOC_QUERY_DV_TIMINGS: _IOC_TYPE = _IOR!(b'V', 99, v4l2_dv_timings);
pub const VIDIOC_DV_TIMINGS_CAP: _IOC_TYPE = _IOWR!(b'V', 100, v4l2_dv_timings_cap);
pub const VIDIOC_ENUM_FREQ_BANDS: _IOC_TYPE = _IOWR!(b'V', 101, v4l2_frequency_band);
pub const VIDIOC_DBG_G_CHIP_INFO: _IOC_TYPE = _IOWR!(b'V', 102, v4l2_dbg_chip_info);
pub const VIDIOC_QUERY_EXT_CTRL: _IOC_TYPE = _IOWR!(b'V', 103, v4l2_query_ext_ctrl);
pub const VIDIOC_REMOVE_BUFS: _IOC_TYPE = _IOWR!(b'V', 104, v4l2_remove_buffers);

#[cfg(test)]
mod tests {
	use super::*;

	/// The values `videodev2.h` expands to. A wrong code is `ENOTTY` at
	/// runtime on whichever board reaches it first, so the shifts, the
	/// direction bits, and the generated struct sizes are pinned here.
	#[test]
	fn codes_match_the_header() {
		// The codes are `c_ulong` (`c_int` on musl), so compare as `u32`.
		assert_eq!(VIDIOC_QUERYCAP as u32, 0x8068_5600);
		// `v4l2_window` drops pointer padding: a 208-byte format shrinks to 204.
		#[cfg(target_pointer_width = "64")]
		assert_eq!(VIDIOC_S_FMT as u32, 0xc0d0_5605);
		#[cfg(target_pointer_width = "32")]
		assert_eq!(VIDIOC_S_FMT as u32, 0xc0cc_5605);
		assert_eq!(VIDIOC_STREAMON as u32, 0x4004_5612);
		// `timespec` shrinks 16 -> 8 on ILP32 and `value64` blocks to the ABI's
		// `u64` alignment: a 136-byte event becomes 128 (align 8, e.g. arm) or
		// 120 (align 4, e.g. x86).
		#[cfg(target_pointer_width = "64")]
		assert_eq!(VIDIOC_DQEVENT as u32, 0x8088_5659);
		#[cfg(all(target_pointer_width = "32", target_arch = "x86"))]
		assert_eq!(VIDIOC_DQEVENT as u32, 0x8078_5659);
		#[cfg(all(target_pointer_width = "32", not(target_arch = "x86")))]
		assert_eq!(VIDIOC_DQEVENT as u32, 0x8080_5659);
		assert_eq!(VIDIOC_SUBSCRIBE_EVENT as u32, 0x4020_565a);
		assert_eq!(VIDIOC_G_SELECTION as u32, 0xc040_565e);
		assert_eq!(VIDIOC_DECODER_CMD as u32, 0xc048_5660);
	}
}
