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
