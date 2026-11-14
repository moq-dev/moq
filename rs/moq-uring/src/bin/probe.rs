//! Exits nonzero, naming why, when this host cannot construct an io_uring worker.
//!
//! The relay benchmarks run it before an io_uring matrix, so an unsupported
//! kernel fails once and legibly instead of in every relay start.

#[cfg(target_os = "linux")]
fn main() {
	use std::error::Error as _;

	if let Err(err) = moq_uring::Worker::new(Default::default()) {
		// An `Io` error's message omits its cause, which is the source.
		let mut message = err.to_string();
		let mut source = err.source();
		while let Some(cause) = source {
			message = format!("{message}: {cause}");
			source = cause.source();
		}
		eprintln!("{message}");
		std::process::exit(1);
	}
}

#[cfg(not(target_os = "linux"))]
fn main() {
	eprintln!("io_uring is only available on Linux");
	std::process::exit(1);
}
