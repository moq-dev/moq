//! Sets `cfg(uring)` where the io_uring workers build: Linux on the architectures
//! io-uring ships prebuilt kernel bindings for. Keep the list in step with the
//! `moq-uring` dependency's target in Cargo.toml.
fn main() {
	println!("cargo::rustc-check-cfg=cfg(uring)");
	let os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
	let arch = std::env::var("CARGO_CFG_TARGET_ARCH").unwrap_or_default();
	let bindings = matches!(
		arch.as_str(),
		"x86_64" | "aarch64" | "riscv64" | "loongarch64" | "powerpc64"
	);
	if os == "linux" && bindings {
		println!("cargo::rustc-cfg=uring");
	}
}
