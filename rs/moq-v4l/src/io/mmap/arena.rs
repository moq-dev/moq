use std::{io, mem, ptr::NonNull, slice, sync::Arc};

use crate::buffer;
use crate::device::Handle;
use crate::memory::Memory;
use crate::sys::*;
use crate::v4l2;

/// One buffer mapped into this process, unmapped when dropped.
pub(crate) struct Mapping {
	ptr: NonNull<u8>,
	len: usize,
}

// SAFETY: a mapping is plain memory this value alone owns, like a `Box<[u8]>`.
unsafe impl Send for Mapping {}
// SAFETY: shared access only hands out `&[u8]`.
unsafe impl Sync for Mapping {}

impl Mapping {
	/// Takes ownership of `len` bytes mapped at `ptr`.
	///
	/// # Safety
	///
	/// `ptr` must start a readable and writable mapping of `len` bytes that
	/// nothing else uses or unmaps.
	unsafe fn new(ptr: NonNull<u8>, len: usize) -> Self {
		Self { ptr, len }
	}

	pub(crate) fn as_slice(&self) -> &[u8] {
		// SAFETY: `new`'s contract, and `&self` excludes a live `&mut`.
		unsafe { slice::from_raw_parts(self.ptr.as_ptr(), self.len) }
	}

	pub(crate) fn as_mut_slice(&mut self) -> &mut [u8] {
		// SAFETY: `new`'s contract, and `&mut self` makes the borrow unique.
		unsafe { slice::from_raw_parts_mut(self.ptr.as_ptr(), self.len) }
	}
}

impl Drop for Mapping {
	fn drop(&mut self) {
		// munmap fails only on an address range that is not a mapping, which
		// `new`'s contract rules out; there is nothing to do about it here.
		let _ = unsafe { v4l2::munmap(self.ptr.as_ptr().cast(), self.len) };
	}
}

/// The buffers of one queue, mapped into this process.
///
/// Dropping it unmaps every buffer, then asks the driver to free them.
pub(crate) struct Arena {
	handle: Arc<Handle>,
	bufs: Vec<Mapping>,
	buf_type: buffer::Type,
	/// Whether the driver holds buffers that a zero-count REQBUFS frees.
	requested: bool,
}

impl Arena {
	pub(crate) fn new(handle: Arc<Handle>, buf_type: buffer::Type) -> Self {
		Arena {
			handle,
			bufs: Vec::new(),
			buf_type,
			requested: false,
		}
	}

	/// The number of mapped buffers.
	pub(crate) fn len(&self) -> usize {
		self.bufs.len()
	}

	/// The buffer at `index`.
	pub(crate) fn get(&self, index: usize) -> &[u8] {
		self.bufs[index].as_slice()
	}

	/// The buffer at `index`, writable.
	pub(crate) fn get_mut(&mut self, index: usize) -> &mut [u8] {
		self.bufs[index].as_mut_slice()
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

	/// Asks the driver for `count` buffers and maps each one, returning how
	/// many it granted. A failure partway leaves what it mapped for drop to undo.
	pub(crate) fn allocate(&mut self, count: u32) -> io::Result<u32> {
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
		self.requested = true;

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

				let len = v4l2_buf.length as usize;
				let ptr = v4l2::mmap(
					std::ptr::null_mut(),
					len,
					libc::PROT_READ | libc::PROT_WRITE,
					libc::MAP_SHARED,
					self.handle.fd(),
					v4l2_buf.m.offset as libc::off_t,
				)?;
				let ptr = NonNull::new(ptr.cast()).ok_or_else(|| io::Error::other("mmap returned null"))?;
				// SAFETY: a fresh MAP_SHARED read/write mapping of `len` bytes.
				self.bufs.push(Mapping::new(ptr, len));
			}
		}

		Ok(v4l2_reqbufs.count)
	}
}

impl Drop for Arena {
	fn drop(&mut self) {
		// The driver frees a buffer only once nothing maps it.
		self.bufs.clear();

		if !self.requested {
			return;
		}

		let mut v4l2_reqbufs = v4l2_requestbuffers {
			count: 0,
			..self.requestbuffers_desc()
		};
		// Fails once the device is gone (ENODEV), and then the kernel frees the
		// buffers itself when the last fd closes.
		let _ = unsafe {
			v4l2::ioctl(
				self.handle.fd(),
				v4l2::vidioc::VIDIOC_REQBUFS,
				&mut v4l2_reqbufs as *mut _ as *mut std::os::raw::c_void,
			)
		};
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	/// An anonymous mapping of `len` bytes, standing in for a device buffer.
	fn anonymous(len: usize) -> Mapping {
		let ptr = unsafe {
			libc::mmap(
				std::ptr::null_mut(),
				len,
				libc::PROT_READ | libc::PROT_WRITE,
				libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
				-1,
				0,
			)
		};
		assert_ne!(ptr, libc::MAP_FAILED);
		unsafe { Mapping::new(NonNull::new(ptr.cast()).unwrap(), len) }
	}

	/// Whether any page of `[ptr, ptr + len)` is mapped.
	fn mapped(ptr: *mut u8, len: usize) -> bool {
		let pages = len.div_ceil(4096);
		let mut residency = vec![0u8; pages];
		unsafe { libc::mincore(ptr.cast(), len, residency.as_mut_ptr()) == 0 }
	}

	#[test]
	fn mapping_unmaps_once_on_drop() {
		let mut mapping = anonymous(8192);
		mapping.as_mut_slice().fill(7);
		assert!(mapping.as_slice().iter().all(|&b| b == 7));

		let (ptr, len) = (mapping.ptr.as_ptr(), mapping.len);
		assert!(mapped(ptr, len));
		drop(mapping);
		assert!(!mapped(ptr, len));
	}

	#[test]
	fn arena_drop_unmaps_every_buffer() {
		let dev_null = Arc::new(Handle::new(
			v4l2::open("/dev/null", libc::O_RDWR).expect("open /dev/null"),
		));
		let mut arena = Arena::new(dev_null, buffer::Type::VideoCapture);
		// /dev/null is no V4L2 device, so it cannot grant buffers.
		assert!(arena.allocate(4).is_err());
		assert!(!arena.requested);

		arena.bufs.push(anonymous(4096));
		arena.bufs.push(anonymous(8192));
		let held: Vec<_> = arena.bufs.iter().map(|m| (m.ptr.as_ptr(), m.len)).collect();
		assert_eq!(arena.len(), 2);
		assert_eq!(arena.get(1).len(), 8192);

		drop(arena);
		for (ptr, len) in held {
			assert!(!mapped(ptr, len));
		}
	}
}
