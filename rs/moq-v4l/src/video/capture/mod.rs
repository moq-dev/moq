pub mod parameters;
pub use parameters::Parameters;

use std::convert::TryFrom;
use std::{io, mem};

use crate::buffer::Type;
use crate::device::Device;
use crate::format::FourCC;
use crate::format::{Description as FormatDescription, Format};
use crate::frameinterval::FrameInterval;
use crate::framesize::FrameSize;
use crate::sys::*;
use crate::v4l2;
use crate::video::traits::Capture;

impl Capture for Device {
	impl_enum_frameintervals!();
	impl_enum_framesizes!();
	impl_enum_formats!(Type::VideoCapture);
	impl_format!(Type::VideoCapture);
	impl_set_format!(Type::VideoCapture);

	fn params(&self) -> io::Result<Parameters> {
		unsafe {
			let mut v4l2_params = v4l2_streamparm {
				type_: Type::VideoCapture as u32,
				..mem::zeroed()
			};
			v4l2::ioctl(
				self.handle().fd(),
				v4l2::vidioc::VIDIOC_G_PARM,
				&mut v4l2_params as *mut _ as *mut std::os::raw::c_void,
			)?;

			Ok(Parameters::from(v4l2_params.parm.capture))
		}
	}

	fn set_params(&self, params: &Parameters) -> io::Result<Parameters> {
		unsafe {
			let mut v4l2_params = v4l2_streamparm {
				type_: Type::VideoCapture as u32,
				parm: v4l2_streamparm__bindgen_ty_1 {
					capture: (*params).into(),
				},
			};
			v4l2::ioctl(
				self.handle().fd(),
				v4l2::vidioc::VIDIOC_S_PARM,
				&mut v4l2_params as *mut _ as *mut std::os::raw::c_void,
			)?;
		}

		self.params()
	}
}
