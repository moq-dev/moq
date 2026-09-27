/// Why a worker or socket could not be set up or has failed.
#[derive(thiserror::Error, Debug)]
#[non_exhaustive]
pub enum Error {
	/// The kernel cannot run this worker at all; there is no fallback here.
	/// The message names the missing feature and the running kernel. Callers
	/// that want a fallback should construct a tokio-based stack instead.
	#[error("unsupported kernel: {0}")]
	Unsupported(String),

	/// An io_uring or socket operation failed.
	#[error("io error: {0}")]
	Io(#[from] std::io::Error),
}

impl Error {
	/// A failed ring setup or registration, naming the locked-memory limit when
	/// that is what ran out.
	///
	/// The kernel charges every ring and provided-buffer ring to the user's
	/// `RLIMIT_MEMLOCK`, shared with every other io_uring that user runs, so a
	/// bare `ENOMEM` points at the wrong culprit.
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
				"{err}: io_uring memory counts against RLIMIT_MEMLOCK ({limit}), shared by every process of this user; \
				 raise it with `ulimit -l` or systemd's `LimitMEMLOCK=`"
			),
		))
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn a_ring_enomem_names_the_memlock_limit() {
		let err = Error::ring(std::io::Error::from_raw_os_error(libc::ENOMEM));
		assert!(err.to_string().contains("RLIMIT_MEMLOCK"), "{err}");

		let err = Error::ring(std::io::Error::from_raw_os_error(libc::EBADF));
		assert!(!err.to_string().contains("RLIMIT_MEMLOCK"), "{err}");
	}
}
