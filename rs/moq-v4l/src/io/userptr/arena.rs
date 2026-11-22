use std::{io, mem, sync::Arc};

use crate::buffer;
use crate::device::Handle;
use crate::memory::Memory;
use crate::sys::*;
use crate::v4l2;

/// The buffers of one queue, allocated by this process and handed to the driver.
///
/// Dropping it asks the driver to let go of them before they are freed.
pub(crate) struct Arena {
	handle: Arc<Handle>,
	pub(crate) bufs: Vec<Vec<u8>>,
	buf_type: buffer::Type,
}

impl Arena {
	pub(crate) fn new(handle: Arc<Handle>, buf_type: buffer::Type) -> Self {
		Arena {
			handle,
			bufs: Vec::new(),
			buf_type,
		}
	}

	fn requestbuffers_desc(&self) -> v4l2_requestbuffers {
		v4l2_requestbuffers {
			type_: self.buf_type as u32,
			memory: Memory::UserPtr as u32,
			..unsafe { mem::zeroed() }
		}
	}

	pub(crate) fn allocate(&mut self, count: u32) -> io::Result<u32> {
		// we need to get the maximum buffer size from the format first
		let mut v4l2_fmt = v4l2_format {
			type_: self.buf_type as u32,
			..unsafe { mem::zeroed() }
		};
		unsafe {
			v4l2::ioctl(
				self.handle.fd(),
				v4l2::vidioc::VIDIOC_G_FMT,
				&mut v4l2_fmt as *mut _ as *mut std::os::raw::c_void,
			)?;
		}

		let mut v4l2_reqbufs = v4l2_requestbuffers {
			count,
			..self.requestbuffers_desc()
		};
		unsafe {
			v4l2::ioctl(
				self.handle.fd(),
				v4l2::vidioc::VIDIOC_REQBUFS,
				&mut v4l2_reqbufs as *mut _ as *mut std::os::raw::c_void,
			)?;
		}

		// allocate the new user buffers
		self.bufs.resize(v4l2_reqbufs.count as usize, Vec::new());
		for i in 0..v4l2_reqbufs.count {
			let buf = &mut self.bufs[i as usize];
			buf.resize(unsafe { v4l2_fmt.fmt.pix.sizeimage } as usize, 0);
		}

		Ok(v4l2_reqbufs.count)
	}
}

impl Drop for Arena {
	fn drop(&mut self) {
		if self.bufs.is_empty() {
			return;
		}

		// A zero-count REQBUFS stops the queue and makes the driver let go of
		// every pointer it was handed.
		let mut v4l2_reqbufs = v4l2_requestbuffers {
			count: 0,
			..self.requestbuffers_desc()
		};
		let released = unsafe {
			v4l2::ioctl(
				self.handle.fd(),
				v4l2::vidioc::VIDIOC_REQBUFS,
				&mut v4l2_reqbufs as *mut _ as *mut std::os::raw::c_void,
			)
		};
		if released.is_err() {
			// The driver may still write into a buffer it was handed, so freeing
			// it would hand that memory to the next allocation. Leak it instead.
			mem::forget(mem::take(&mut self.bufs));
		}
	}
}
