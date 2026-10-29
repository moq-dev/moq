//! The synchronization primitives kio is built on, swappable for loom's.
//!
//! Under `--cfg loom` these resolve to `loom::sync`, which records every atomic
//! and lock operation so the model checker can permute the interleavings. Normal
//! builds get `std::sync` and this module compiles away.
//!
//! Loom's `Arc` has no `downgrade`, so the two places that need a `Weak` keep std's:
//! [`crate::lock`] (for `WeakLock`) and [`crate::waiter`] (whose list holds weak waker
//! handles). Both still hold loom's `Mutex`, which is what orders one handle against
//! another; only the refcount itself goes unpermuted. The `Arc<Counts>` that carries
//! the producer/consumer counts is loom's.
//!
//! [`crate::log`] frees segments through `crossbeam-epoch`, which `--cfg crossbeam_loom`
//! puts under loom as well.

#[cfg(loom)]
pub(crate) use loom::{
	cell::UnsafeCell,
	sync::{
		Arc, Mutex, MutexGuard,
		atomic::{AtomicBool, AtomicPtr, AtomicU64, AtomicUsize, fence},
	},
};
#[cfg(not(loom))]
pub(crate) use std::sync::{
	Arc, Mutex, MutexGuard,
	atomic::{AtomicBool, AtomicPtr, AtomicU64, AtomicUsize, fence},
};

/// `std::cell::UnsafeCell` behind loom's closure API, so loom can check every access.
#[cfg(not(loom))]
#[derive(Debug)]
pub(crate) struct UnsafeCell<T>(std::cell::UnsafeCell<T>);

#[cfg(not(loom))]
impl<T> UnsafeCell<T> {
	pub(crate) const fn new(value: T) -> Self {
		Self(std::cell::UnsafeCell::new(value))
	}

	pub(crate) fn with<R>(&self, f: impl FnOnce(*const T) -> R) -> R {
		f(self.0.get())
	}

	pub(crate) fn with_mut<R>(&self, f: impl FnOnce(*mut T) -> R) -> R {
		f(self.0.get())
	}
}
