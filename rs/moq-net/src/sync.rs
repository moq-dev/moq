//! Mutexes that let Loom schedule a contending thread instead of blocking its executor.

#[cfg(loom)]
pub(crate) use loom::sync::{Mutex, MutexGuard};
#[cfg(not(loom))]
pub(crate) use std::sync::{Mutex, MutexGuard};
