//! Auto-generated bindings to NVIDIA Video Codec SDK.
//!
//! The bindings were generated using [bindgen](https://github.com/rust-lang/rust-bindgen)
//! using the scripts `sys/linux_sys/bindgen.sh` and
//! `sys/windows_sys/bindgen.ps1` for the respective operating system.

mod guid;
mod version;

// The bindgen output is plain C-ABI type/enum/fn-pointer definitions, so the
// "linux" bindings compile on any non-Windows target. On macOS the crate only
// needs to compile (NVENC is never actually loaded there), so reuse them.
#[allow(warnings)]
#[rustfmt::skip]
#[cfg(not(target_os = "windows"))]
mod linux_sys;
#[cfg(not(target_os = "windows"))]
pub use linux_sys::*;

#[allow(warnings)]
#[rustfmt::skip]
#[cfg(target_os = "windows")]
mod windows_sys;
#[cfg(target_os = "windows")]
pub use windows_sys::*;
