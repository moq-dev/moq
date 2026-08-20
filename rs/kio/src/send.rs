//! A `Send` bound that disappears on wasm.

/// `Send` on native targets, no bound at all on wasm.
///
/// Browser wasm is single-threaded, and the futures and JS values that flow through it
/// are `!Send`. A `Send` bound in a public API therefore excludes types that are
/// perfectly safe on the one thread that exists. Use this wherever the bound is there
/// to allow moving work between native threads, rather than to express a real
/// requirement of the type itself.
///
/// Natively this is implied by `Send`, so an existing `T: Send` caller keeps compiling
/// and a `T: MaybeSend` bound still yields `Send`.
#[cfg(not(target_family = "wasm"))]
pub trait MaybeSend: Send {}

#[cfg(not(target_family = "wasm"))]
impl<T: Send + ?Sized> MaybeSend for T {}

#[cfg(target_family = "wasm")]
pub trait MaybeSend {}

#[cfg(target_family = "wasm")]
impl<T: ?Sized> MaybeSend for T {}
