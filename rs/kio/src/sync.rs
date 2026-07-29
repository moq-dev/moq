//! The synchronization primitives kio is built on, swappable for loom's.
//!
//! Under `--cfg loom` these resolve to `loom::sync`, which records every atomic
//! and lock operation so the model checker can permute the interleavings. Normal
//! builds get `std::sync` and this module compiles away.
//!
//! Loom's `Arc` has no `downgrade`, so the two places that need a `Weak` keep
//! std's: [`crate::lock`] (for `WeakLock`) and [`crate::waiter`] (whose list holds
//! weak waker handles). Both still hold loom's `Mutex`, which is what orders one
//! handle against another; only the refcount itself goes unpermuted. The
//! `Arc<Counts>` that carries the producer/consumer counts is loom's.

#[cfg(loom)]
pub(crate) use loom::sync::{Arc, Mutex, MutexGuard, atomic::AtomicUsize};
#[cfg(not(loom))]
pub(crate) use std::sync::{Arc, Mutex, MutexGuard, atomic::AtomicUsize};
