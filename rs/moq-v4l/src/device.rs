use std::os::fd::{AsRawFd, OwnedFd};
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
///
/// Dropping the last one closes the device.
pub struct Handle {
	fd: OwnedFd,
}

impl Handle {
	pub(crate) fn new(fd: OwnedFd) -> Self {
		Self { fd }
	}

	/// Returns the raw file descriptor
	pub fn fd(&self) -> std::os::raw::c_int {
		self.fd.as_raw_fd()
	}

	/// Polls the file descriptor for I/O events
	///
	/// # Arguments
	///
	/// * `events`  - The events you are interested in (e.g. POLLIN)
	///
	/// * `timeout` - Timeout in milliseconds. A value of zero returns immediately,
	///   even if the fd is not ready. A negative value means infinite timeout (blocking).
	///
	/// A signal that interrupts a finite wait ends it as a timeout, returning
	/// zero early rather than restarting and outliving the timeout; an
	/// infinite wait restarts.
	pub fn poll(&self, events: i16, timeout: i32) -> io::Result<i32> {
		loop {
			let mut fds = [libc::pollfd {
				fd: self.fd(),
				events,
				revents: 0,
			}];
			match unsafe { libc::poll(fds.as_mut_ptr(), 1, timeout) } {
				-1 => {
					let err = io::Error::last_os_error();
					if err.kind() != io::ErrorKind::Interrupted {
						return Err(err);
					}
					if timeout >= 0 {
						return Ok(0);
					}
				}
				// Zero means the wait timed out, one that the fd saw activity.
				ret => return Ok(ret),
			}
		}
	}
}

#[cfg(test)]
mod tests {
	use std::os::fd::FromRawFd;
	use std::sync::mpsc;

	use super::*;

	/// Signals that keep arriving must not stretch a finite wait past its
	/// timeout, which a restart with the full timeout would.
	#[test]
	fn an_interrupted_wait_ends_as_a_timeout() {
		extern "C" fn ignore(_: libc::c_int) {}
		unsafe { libc::signal(libc::SIGUSR1, ignore as *const () as libc::sighandler_t) };

		let mut fds = [0; 2];
		assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0);
		// An empty pipe never becomes readable while its write end stays open.
		let (read, _write) = unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) };
		let handle = Handle::new(read);

		let (tx, rx) = mpsc::channel();
		let waiter = std::thread::spawn(move || {
			tx.send(unsafe { libc::pthread_self() }).unwrap();
			handle.poll(libc::POLLIN, 60_000)
		});
		let thread = rx.recv().unwrap();

		for _ in 0..1_000_000 {
			if waiter.is_finished() {
				break;
			}
			unsafe { libc::pthread_kill(thread, libc::SIGUSR1) };
			std::thread::yield_now();
		}
		assert!(waiter.is_finished(), "the wait outlived a million signals");
		assert_eq!(waiter.join().unwrap().unwrap(), 0);
	}
}
