//! This crate has been renamed to `moq-tokio`.

// Cargo sets this variable only when checking the package directly. Dependency
// builds fail while release tooling can still verify and publish the tombstone.
const _: () = assert!(
	option_env!("CARGO_PRIMARY_PACKAGE").is_some(),
	"`moq-native` has been renamed to `moq-tokio`. Replace `moq-native` with `moq-tokio` in Cargo.toml and `moq_native` with `moq_tokio` in Rust source."
);
