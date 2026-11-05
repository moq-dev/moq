use std::path::Path;
use std::sync::Arc;
use std::{io, mem};

use libc;

use crate::capability::Capabilities;
use crate::sys::*;
use crate::v4l2;

/// Linux capture device abstraction
pub struct Device {
	/// Raw handle
	handle: Arc<Handle>,
}

impl Device {
	/// Returns a capture device by index
	///
	/// Devices are usually enumerated by the system.
	/// An index of zero thus represents the first device the system got to know about.
	///
	/// # Arguments
	///
	/// * `index` - Index (0: first, 1: second, ..)
	///
	/// # Example
	///
	/// ```
	/// use moq_v4l::device::Device;
	/// let dev = Device::new(0);
	/// ```
	pub fn new(index: usize) -> io::Result<Self> {
		let path = format!("{}{}", "/dev/video", index);
		let fd = v4l2::open(path, libc::O_RDWR | libc::O_NONBLOCK)?;

		if fd == -1 {
			return Err(io::Error::last_os_error());
		}

		Ok(Device {
			handle: Arc::new(Handle::new(fd)),
		})
	}

	/// Returns a capture device by path
	///
	/// Linux device nodes are usually found in /dev/videoX or /sys/class/video4linux/videoX.
	///
	/// # Arguments
	///
	/// * `path` - Path (e.g. "/dev/video0")
	///
	/// # Example
	///
	/// ```
	/// use moq_v4l::device::Device;
	/// let dev = Device::with_path("/dev/video0");
	/// ```
	pub fn with_path<P: AsRef<Path>>(path: P) -> io::Result<Self> {
		let fd = v4l2::open(&path, libc::O_RDWR | libc::O_NONBLOCK)?;

		if fd == -1 {
			return Err(io::Error::last_os_error());
		}

		Ok(Device {
			handle: Arc::new(Handle::new(fd)),
		})
	}

	/// Returns the raw device handle
	pub fn handle(&self) -> Arc<Handle> {
		self.handle.clone()
	}

	/// Returns video4linux framework defined information such as card, driver, etc.
	pub fn query_caps(&self) -> io::Result<Capabilities> {
		unsafe {
			let mut v4l2_caps: v4l2_capability = mem::zeroed();
			v4l2::ioctl(
				self.handle().fd(),
				v4l2::vidioc::VIDIOC_QUERYCAP,
				&mut v4l2_caps as *mut _ as *mut std::os::raw::c_void,
			)?;

			Ok(Capabilities::from(v4l2_caps))
		}
	}
}

impl io::Read for Device {
	fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
		unsafe {
			let ret = libc::read(
				self.handle().fd(),
				buf.as_mut_ptr() as *mut std::os::raw::c_void,
				buf.len(),
			);
			match ret {
				-1 => Err(io::Error::last_os_error()),
				ret => Ok(ret as usize),
			}
		}
	}
}

impl io::Write for Device {
	fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
		unsafe {
			let ret = libc::write(
				self.handle().fd(),
				buf.as_ptr() as *const std::os::raw::c_void,
				buf.len(),
			);

			match ret {
				-1 => Err(io::Error::last_os_error()),
				ret => Ok(ret as usize),
			}
		}
	}

	fn flush(&mut self) -> io::Result<()> {
		// write doesn't use a buffer, so it effectively flushes with each call
		// therefore, we don't have anything to flush later
		Ok(())
	}
}

/// Device handle for low-level access.
///
/// Acquiring a handle facilitates (possibly mutating) interactions with the device.
pub struct Handle {
	fd: std::os::raw::c_int,
}

impl Handle {
	fn new(fd: std::os::raw::c_int) -> Self {
		Self { fd }
	}

	/// Returns the raw file descriptor
	pub fn fd(&self) -> std::os::raw::c_int {
		self.fd
	}

	/// Polls the file descriptor for I/O events
	///
	/// # Arguments
	///
	/// * `events`  - The events you are interested in (e.g. POLLIN)
	///
	/// * `timeout` - Timeout in milliseconds. A value of zero returns immediately,
	///   even if the fd is not ready. A negative value means infinite timeout (blocking).
	pub fn poll(&self, events: i16, timeout: i32) -> io::Result<i32> {
		match unsafe {
			libc::poll(
				[libc::pollfd {
					fd: self.fd,
					events,
					revents: 0,
				}]
				.as_mut_ptr(),
				1,
				timeout,
			)
		} {
			-1 => Err(io::Error::last_os_error()),
			ret => {
				// A return value of zero means that we timed out. A positive value signifies the
				// number of fds with non-zero revents fields (aka I/O activity).
				assert!(ret == 0 || ret == 1);
				Ok(ret)
			}
		}
	}
}

impl Drop for Handle {
	fn drop(&mut self) {
		v4l2::close(self.fd).unwrap();
	}
}
