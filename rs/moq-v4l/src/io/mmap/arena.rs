use std::{io, mem, ptr, slice, sync::Arc};

use crate::buffer;
use crate::device::Handle;
use crate::memory::Memory;
use crate::sys::*;
use crate::v4l2;

/// Manage mapped buffers
///
/// All buffers are unmapped in the Drop impl.
/// In case of errors during unmapping, we panic because there is memory corruption going on.
pub struct Arena<'a> {
	handle: Arc<Handle>,
	pub bufs: Vec<&'a mut [u8]>,
	pub buf_type: buffer::Type,
}

impl<'a> Arena<'a> {
	/// Returns a new buffer manager instance
	///
	/// You usually do not need to use this directly.
	/// A MappedBufferStream creates its own manager instance by default.
	///
	/// # Arguments
	///
	/// * `handle` - Device handle to get its file descriptor
	/// * `buf_type` - Type of the buffers
	pub fn new(handle: Arc<Handle>, buf_type: buffer::Type) -> Self {
		Arena {
			handle,
			bufs: Vec::new(),
			buf_type,
		}
	}

	fn buffer_desc(&self) -> v4l2_buffer {
		v4l2_buffer {
			type_: self.buf_type as u32,
			memory: Memory::Mmap as u32,
			..unsafe { mem::zeroed() }
		}
	}

	fn requestbuffers_desc(&self) -> v4l2_requestbuffers {
		v4l2_requestbuffers {
			type_: self.buf_type as u32,
			memory: Memory::Mmap as u32,
			..unsafe { mem::zeroed() }
		}
	}

	pub fn allocate(&mut self, count: u32) -> io::Result<u32> {
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

		for index in 0..v4l2_reqbufs.count {
			let mut v4l2_buf = v4l2_buffer {
				index,
				..self.buffer_desc()
			};
			unsafe {
				v4l2::ioctl(
					self.handle.fd(),
					v4l2::vidioc::VIDIOC_QUERYBUF,
					&mut v4l2_buf as *mut _ as *mut std::os::raw::c_void,
				)?;

				let ptr = v4l2::mmap(
					ptr::null_mut(),
					v4l2_buf.length as usize,
					libc::PROT_READ | libc::PROT_WRITE,
					libc::MAP_SHARED,
					self.handle.fd(),
					v4l2_buf.m.offset as libc::off_t,
				)?;

				let slice = slice::from_raw_parts_mut::<u8>(ptr as *mut u8, v4l2_buf.length as usize);
				self.bufs.push(slice);
			}
		}

		Ok(v4l2_reqbufs.count)
	}

	pub fn release(&mut self) -> io::Result<()> {
		for buf in &self.bufs {
			unsafe {
				v4l2::munmap(buf.as_ptr() as *mut core::ffi::c_void, buf.len())?;
			}
		}

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
			)?;
		}

		self.bufs.clear();
		Ok(())
	}
}

impl<'a> Drop for Arena<'a> {
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
