use std::ffi::CString;
use std::os::fd::{FromRawFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::{io, path::Path};

use crate::v4l2::vidioc;

mod detail {
	use crate::v4l2::vidioc;

	pub unsafe fn open(path: *const std::os::raw::c_char, flags: i32) -> std::os::raw::c_int {
		libc::open(path, flags)
	}
	pub unsafe fn ioctl(
		fd: std::os::raw::c_int,
		request: vidioc::_IOC_TYPE,
		argp: *mut std::os::raw::c_void,
	) -> std::os::raw::c_int {
		/*
		 * It turns out the libc crate (and libc itself!) defines ioctl() with
		 * different, incompatible argument types on different platforms. To
		 * hack around this without conditional compilation, use syscall()
		 * instead as a drop-in replacement. Details:
		 * https://github.com/rust-lang/libc/issues/1036
		 */
		libc::syscall(libc::SYS_ioctl, fd, request, argp) as std::os::raw::c_int
	}
	pub unsafe fn mmap(
		start: *mut std::os::raw::c_void,
		length: usize,
		prot: std::os::raw::c_int,
		flags: std::os::raw::c_int,
		fd: std::os::raw::c_int,
		offset: libc::off_t,
	) -> *mut std::os::raw::c_void {
		libc::mmap(start, length, prot, flags, fd, offset)
	}
	pub unsafe fn munmap(start: *mut std::os::raw::c_void, length: usize) -> std::os::raw::c_int {
		libc::munmap(start, length)
	}
}

/// Opens the device node at `path`, closed when the returned fd drops.
///
/// In case of errors, the last OS error will be reported, aka errno on Linux.
/// A path holding a NUL byte fails with [`io::ErrorKind::InvalidInput`].
///
/// # Arguments
///
/// * `path` - Path to the device node
/// * `flags` - Open flags
///
/// # Example
///
/// ```
/// use moq_v4l::v4l2;
///
/// let fd = v4l2::open("/dev/video0", libc::O_RDWR);
/// ```
pub fn open<P: AsRef<Path>>(path: P, flags: i32) -> io::Result<OwnedFd> {
	let c_path = CString::new(path.as_ref().as_os_str().as_bytes())
		.map_err(|err| io::Error::new(io::ErrorKind::InvalidInput, err))?;

	let fd = unsafe { detail::open(c_path.as_ptr(), flags) };
	if fd == -1 {
		Err(io::Error::last_os_error())
	} else {
		// SAFETY: open just returned this fd, and nothing else owns it.
		Ok(unsafe { OwnedFd::from_raw_fd(fd) })
	}
}

/// A convenience wrapper around v4l2_ioctl.
///
/// In case of errors, the last OS error will be reported, aka errno on Linux.
///
/// # Arguments
///
/// * `fd` - File descriptor
/// * `request` - IO control code (see [`vidioc`])
/// * `argp` - Pointer to memory region holding the argument type
///
/// # Safety
///
/// For maximum flexibility, argp must be a raw pointer. Thus, the entire function is unsafe.
///
/// # Example
///
/// ```
/// use std::mem;
/// use std::os::fd::AsRawFd;
///
/// use moq_v4l::sys::*;
/// use moq_v4l::v4l2;
///
/// let fd = v4l2::open("/dev/video0", libc::O_RDWR);
/// let mut v4l2_caps: v4l2_capability;
/// unsafe {
///     v4l2_caps = mem::zeroed();
/// }
///
/// if let Ok(fd) = fd {
///     unsafe {
///         v4l2::ioctl(fd.as_raw_fd(), v4l2::vidioc::VIDIOC_QUERYCAP,
///                     &mut v4l2_caps as *mut _ as *mut std::os::raw::c_void);
///     }
/// }
/// ```
pub unsafe fn ioctl(
	fd: std::os::raw::c_int,
	request: vidioc::_IOC_TYPE,
	argp: *mut std::os::raw::c_void,
) -> io::Result<()> {
	let ret = detail::ioctl(fd, request, argp);

	if ret == -1 {
		Err(io::Error::last_os_error())
	} else {
		Ok(())
	}
}

/// A convenience wrapper around v4l2_mmap.
///
/// In case of errors, the last OS error will be reported, aka errno on Linux.
///
/// # Arguments
///
/// * `start` - Starting address of the new mapping, usually NULL
/// * `length` - Length of the mapped region
/// * `prot` - Desired memory protection of the mapped region
/// * `flags` - Mapping flags
/// * `fd` - File descriptor representing an opened device
/// * `offset` - Offset in the source region, usually 0
///
/// # Safety
///
/// Start must be a raw pointer. Thus, the entire function is unsafe.
///
/// # Example
///
/// ```
/// use std::os::fd::AsRawFd;
/// use std::ptr;
/// use moq_v4l::v4l2;
///
/// let fd = v4l2::open("/dev/video0", libc::O_RDWR);
/// if let Ok(fd) = fd {
///     /* VIDIOC_REQBUFS */
///     /* VIDIOC_QUERYBUF */
///     let mapping_length: usize = 1000;
///
///     unsafe {
///         let mapping = v4l2::mmap(ptr::null_mut(), mapping_length,
///                                  libc::PROT_READ | libc::PROT_WRITE,
///                                  libc::MAP_SHARED, fd.as_raw_fd(), 0);
///     }
/// }
/// ```
pub unsafe fn mmap(
	start: *mut std::os::raw::c_void,
	length: usize,
	prot: std::os::raw::c_int,
	flags: std::os::raw::c_int,
	fd: std::os::raw::c_int,
	offset: libc::off_t,
) -> io::Result<*mut std::os::raw::c_void> {
	let ret = detail::mmap(start, length, prot, flags, fd, offset);
	if ret as usize == usize::MAX {
		Err(io::Error::last_os_error())
	} else {
		Ok(ret)
	}
}

/// A convenience wrapper around v4l2_munmap.
///
/// In case of errors, the last OS error will be reported, aka errno on Linux.
///
/// # Arguments
///
/// * `start` - Starting address of the mapping
/// * `length` - Length of the mapped region
///
/// # Safety
///
/// Start must be a raw pointer. Thus, the entire function is unsafe.
///
/// # Example
///
/// ```
/// use std::os::fd::AsRawFd;
/// use std::ptr;
/// use moq_v4l::v4l2;
///
/// let fd = v4l2::open("/dev/video0", libc::O_RDWR);
/// if let Ok(fd) = fd {
///     /* VIDIOC_REQBUFS */
///     /* VIDIOC_QUERYBUF */
///     let mapping_length: usize = 1000;
///
///     unsafe {
///         let mapping = v4l2::mmap(ptr::null_mut(), mapping_length,
///                                  libc::PROT_READ | libc::PROT_WRITE,
///                                  libc::MAP_SHARED, fd.as_raw_fd(), 0);
///         if let Ok(mapping) = mapping {
///             v4l2::munmap(mapping, mapping_length).unwrap();
///         }
///     }
/// }
/// ```
pub unsafe fn munmap(start: *mut std::os::raw::c_void, length: usize) -> io::Result<()> {
	let ret = detail::munmap(start, length);
	if ret == -1 {
		Err(io::Error::last_os_error())
	} else {
		Ok(())
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn open_rejects_interior_nul() {
		let err = open("/dev/vid\0eo0", libc::O_RDWR).unwrap_err();
		assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
	}
}
