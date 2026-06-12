//! Send-agnostic future boxing.
//!
//! Native transports (Quinn) are `Send`, so boxed futures use the usual
//! `Send`-bound `BoxFuture`. Browser WebTransport is `!Send`, so on wasm we box
//! without the bound via `LocalBoxFuture`. `MaybeSendBox` resolves to the right
//! one per target, and `.maybe_boxed()` picks `boxed()` vs `boxed_local()`.

use std::future::Future;

use futures::FutureExt;

#[cfg(not(target_family = "wasm"))]
pub(crate) type MaybeSendBox<'a, T> = futures::future::BoxFuture<'a, T>;
#[cfg(target_family = "wasm")]
pub(crate) type MaybeSendBox<'a, T> = futures::future::LocalBoxFuture<'a, T>;

#[cfg(not(target_family = "wasm"))]
pub(crate) trait MaybeBoxedExt<'a>: Future + Send + Sized + 'a {
	fn maybe_boxed(self) -> MaybeSendBox<'a, Self::Output> {
		self.boxed()
	}
}
#[cfg(not(target_family = "wasm"))]
impl<'a, F: Future + Send + 'a> MaybeBoxedExt<'a> for F {}

#[cfg(target_family = "wasm")]
pub(crate) trait MaybeBoxedExt<'a>: Future + Sized + 'a {
	fn maybe_boxed(self) -> MaybeSendBox<'a, Self::Output> {
		self.boxed_local()
	}
}
#[cfg(target_family = "wasm")]
impl<'a, F: Future + 'a> MaybeBoxedExt<'a> for F {}
