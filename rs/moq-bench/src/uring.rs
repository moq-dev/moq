//! Checks whether the host can construct the io_uring worker used by runtime benchmarks.

#[cfg(target_os = "linux")]
fn main() {
	if let Err(err) = moq_uring::Worker::new(Default::default()) {
		eprintln!("{err}");
		std::process::exit(1);
	}
}

#[cfg(not(target_os = "linux"))]
fn main() {
	eprintln!("io_uring is only available on Linux");
	std::process::exit(1);
}
