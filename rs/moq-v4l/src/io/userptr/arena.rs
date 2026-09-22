use std::{io, mem, sync::Arc};

use crate::buffer;
use crate::device::Handle;
use crate::memory::Memory;
use crate::sys::*;
use crate::v4l2;

/// Manage user allocated buffers
///
/// All buffers are released in the Drop impl.
pub struct Arena {
	handle: Arc<Handle>,
	pub bufs: Vec<Vec<u8>>,
	pub buf_type: buffer::Type,
}

impl Arena {
	/// Returns a new buffer manager instance
	///
	/// You usually do not need to use this directly.
	/// A UserBufferStream creates its own manager instance by default.
	///
	/// # Arguments
	///
	/// * `dev` - Device handle to get its file descriptor
	/// * `buf_type` - Type of the buffers
	pub fn new(handle: Arc<Handle>, buf_type: buffer::Type) -> Self {
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

	pub fn allocate(&mut self, count: u32) -> io::Result<u32> {
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
			unsafe {
				buf.resize(v4l2_fmt.fmt.pix.sizeimage as usize, 0);
			}
		}

		Ok(v4l2_reqbufs.count)
	}

	pub fn release(&mut self) -> io::Result<()> {
		// free all buffers by requesting 0
		let mut v4l2_reqbufs = v4l2_requestbuffers {
			count: 0,
			..self.requestbuffers_desc()
		};
		unsafe {
			v4l2::ioctl(
				self.handle.fd(),
				v4l2::vidioc::VIDIOC_REQBUFS,
				&mut v4l2_reqbufs as *mut _ as *mut std::os::raw::c_void,
			)
		}
	}
}

impl Drop for Arena {
	fn drop(&mut self) {
		if self.bufs.is_empty() {
			// nothing to do
			return;
		}

		if let Err(e) = self.release() {
			if let Some(code) = e.raw_os_error() {
				// ENODEV means the file descriptor wrapped in the handle became invalid, most
				// likely because the device was unplugged or the connection (USB, PCI, ..)
				// broke down. Handle this case gracefully by ignoring it.
				if code == 19 {
					/* ignore */
					return;
				}
			}

			panic!("{:?}", e)
		}
	}
}
