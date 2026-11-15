/// Why a worker or socket could not be set up or has failed.
#[derive(thiserror::Error, Debug)]
#[non_exhaustive]
pub enum Error {
	/// The kernel cannot run this worker at all; there is no fallback here.
	/// The message names the missing feature and the running kernel. Callers
	/// that want a fallback should construct a tokio-based stack instead.
	#[error("unsupported kernel: {0}")]
	Unsupported(String),

	/// An io_uring or socket operation failed. The message omits the cause, which
	/// is the source, so `{:#}` and a source chain each show it once.
	#[error("io error")]
	Io(#[from] std::io::Error),
}

impl Error {
	/// A failed ring setup or registration, naming what an `ENOMEM` can mean.
	///
	/// The kernel cannot tell which ran out, and a stable backport makes the
	/// version no guide: the memory cgroup, physically contiguous pages on a
	/// fragmented host, or, from Linux 6.14 until the 7.3 fix, the user's
	/// `RLIMIT_MEMLOCK`, shared with every other io_uring that user runs.
	pub(crate) fn ring(err: std::io::Error) -> Self {
		if err.raw_os_error() != Some(libc::ENOMEM) {
			return Self::Io(err);
		}
		let mut limit = libc::rlimit {
			rlim_cur: 0,
			rlim_max: 0,
		};
		// SAFETY: valid out-pointer.
		let limit = match unsafe { libc::getrlimit(libc::RLIMIT_MEMLOCK, &mut limit) } {
			0 => format!("{} KiB", limit.rlim_cur / 1024),
			_ => "unknown".into(),
		};
		Self::Io(std::io::Error::new(
			err.kind(),
			format!(
				"{err}: the kernel could not allocate the io_uring ring, from the memory cgroup or from contiguous pages \
				 on a fragmented host; Linux 6.14 until the 7.3 fix also charges it to RLIMIT_MEMLOCK ({limit}), shared \
				 by every process of this user, which `ulimit -l` or systemd's `LimitMEMLOCK=` raises"
			),
		))
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn a_ring_enomem_names_each_cause() {
		let err = anyhow::Error::new(Error::ring(std::io::Error::from_raw_os_error(libc::ENOMEM)));
		let msg = format!("{err:#}");
		for cause in ["memory cgroup", "fragmented", "RLIMIT_MEMLOCK"] {
			assert!(msg.contains(cause), "{msg}");
		}

		let err = anyhow::Error::new(Error::ring(std::io::Error::from_raw_os_error(libc::EBADF)));
		assert!(!format!("{err:#}").contains("RLIMIT_MEMLOCK"), "{err:#}");
	}

	#[test]
	fn an_io_error_names_its_cause_once() {
		let err = anyhow::Error::new(Error::from(std::io::Error::other("ring refused")));
		assert_eq!(format!("{err:#}"), "io error: ring refused");
		assert_eq!(err.chain().count(), 2);
	}
}
