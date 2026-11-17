use std::{io, os::fd::AsRawFd};

use crate::udp::{cmsg, imp::set_socket_option};

pub(super) mod gso {
	use super::*;
	use std::{ffi::CStr, mem, str::FromStr, sync::OnceLock};

	// Support for UDP GSO has been added to linux kernel in version 4.18
	// https://github.com/torvalds/linux/commit/cb586c63e3fc5b227c51fd8c4cb40b34d3750645
	const SUPPORTED_SINCE: KernelVersion = KernelVersion {
		version: 4,
		major_revision: 18,
	};

	/// Checks whether GSO support is available
	///
	/// Checks the kernel version followed by setting the UDP_SEGMENT option on a socket.
	pub(crate) fn max_gso_segments(socket: &impl AsRawFd) -> usize {
		const GSO_SIZE: libc::c_int = 1500;

		if !SUPPORTED_BY_CURRENT_KERNEL.get_or_init(supported_by_current_kernel) {
			return 1;
		}

		// As defined in linux/udp.h
		// #define UDP_MAX_SEGMENTS        (1 << 6UL)
		match set_socket_option(socket, libc::SOL_UDP, libc::UDP_SEGMENT, GSO_SIZE) {
			Ok(()) => {
				// Disable GSO again globally to ensure we can selectively enable it via cmsg.
				// See:
				// - https://github.com/quinn-rs/quinn/issues/2575
				// - https://man7.org/linux/man-pages/man7/udp.7.html
				let _ = set_socket_option(socket, libc::SOL_UDP, libc::UDP_SEGMENT, 0);

				64
			}
			Err(_e) => {
				crate::udp::log::debug!(
					"failed to set `UDP_SEGMENT` socket option ({_e}); setting `max_gso_segments = 1`"
				);

				1
			}
		}
	}

	pub(crate) fn set_segment_size(encoder: &mut cmsg::Encoder<'_, libc::msghdr>, segment_size: u16) {
		encoder.push(libc::SOL_UDP, libc::UDP_SEGMENT, segment_size);
	}

	// Avoid calling `supported_by_current_kernel` for each socket by using `OnceLock`.
	static SUPPORTED_BY_CURRENT_KERNEL: OnceLock<bool> = OnceLock::new();

	fn supported_by_current_kernel() -> bool {
		let kernel_version_string = match kernel_version_string() {
			Ok(kernel_version_string) => kernel_version_string,
			Err(_e) => {
				crate::udp::log::warn!("GSO disabled: uname returned {_e}");
				return false;
			}
		};

		let Some(kernel_version) = KernelVersion::from_str(&kernel_version_string) else {
			crate::udp::log::warn!("GSO disabled: failed to parse kernel version ({kernel_version_string})");
			return false;
		};

		if kernel_version < SUPPORTED_SINCE {
			crate::udp::log::info!("GSO disabled: kernel too old ({kernel_version_string}); need 4.18+",);
			return false;
		}

		true
	}

	fn kernel_version_string() -> io::Result<String> {
		let mut n = unsafe { mem::zeroed() };
		let r = unsafe { libc::uname(&mut n) };
		if r != 0 {
			return Err(io::Error::last_os_error());
		}
		Ok(unsafe { CStr::from_ptr(n.release[..].as_ptr()).to_string_lossy().into_owned() })
	}

	// https://www.linfo.org/kernel_version_numbering.html
	#[derive(Eq, PartialEq, Ord, PartialOrd, Debug)]
	struct KernelVersion {
		version: u8,
		major_revision: u8,
	}

	impl KernelVersion {
		fn from_str(release: &str) -> Option<Self> {
			let mut split = release.split_once('-').map(|pair| pair.0).unwrap_or(release).split('.');

			let version = u8::from_str(split.next()?).ok()?;
			let major_revision = u8::from_str(split.next()?).ok()?;

			Some(Self {
				version,
				major_revision,
			})
		}
	}

	#[cfg(test)]
	mod test {
		use super::*;

		#[test]
		fn parse_current_kernel_version_release_string() {
			let release = kernel_version_string().unwrap();
			KernelVersion::from_str(&release).unwrap();
		}

		#[test]
		fn parse_kernel_version_release_string() {
			// These are made up for the test
			assert_eq!(
				KernelVersion::from_str("4.14"),
				Some(KernelVersion {
					version: 4,
					major_revision: 14
				})
			);
			assert_eq!(
				KernelVersion::from_str("4.18"),
				Some(KernelVersion {
					version: 4,
					major_revision: 18
				})
			);
			// These were seen in the wild
			assert_eq!(
				KernelVersion::from_str("4.14.186-27095505"),
				Some(KernelVersion {
					version: 4,
					major_revision: 14
				})
			);
			assert_eq!(
				KernelVersion::from_str("6.8.0-59-generic"),
				Some(KernelVersion {
					version: 6,
					major_revision: 8
				})
			);
		}
	}
}
