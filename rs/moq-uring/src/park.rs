//! Cross-thread wakeups for a parked worker: one futex word, no ring required.
//!
//! The worker parks inside `io_uring_enter` waiting for completions. Remote
//! wakers cannot touch the ring (`SINGLE_ISSUER`), so the worker keeps a
//! `FUTEX_WAIT` SQE armed on this word while parked and a remote wake is a
//! plain atomic store plus a `futex(2)` wake syscall. While the worker is
//! awake the store alone suffices and the syscall is skipped.

use std::sync::{
	Arc,
	atomic::{AtomicU32, Ordering},
};
use std::task::{Wake, Waker};

/// The worker is polling; a wake only needs to prevent the next park.
pub(crate) const RUNNING: u32 = 0;
/// A wake arrived; the next park attempt consumes it and re-polls instead.
pub(crate) const NOTIFIED: u32 = 1;
/// The worker is parked (or committing to park) with a futex wait armed.
pub(crate) const PARKED: u32 = 2;

// futex2 flags for the io_uring FUTEX_WAIT SQE. Not yet in the libc crate.
pub(crate) const FUTEX2_SIZE_U32: u32 = 0x2;
pub(crate) const FUTEX2_PRIVATE: u32 = 128;
/// Matches any wake, including the plain (non-bitset) `FUTEX_WAKE` below.
pub(crate) const FUTEX_BITSET_MATCH_ANY: u64 = u32::MAX as u64;

/// The futex word a worker parks on. `Arc`ed into every waker the worker
/// mints, so a wake from any thread lands here.
pub(crate) struct Unpark {
	pub(crate) word: AtomicU32,
}

impl Unpark {
	pub(crate) fn new() -> Arc<Self> {
		Arc::new(Self {
			word: AtomicU32::new(RUNNING),
		})
	}

	/// Wake the worker: mark the word notified and, only if it was parked,
	/// kick the futex so the armed `FUTEX_WAIT` completes.
	pub(crate) fn unpark(&self) {
		if self.word.swap(NOTIFIED, Ordering::AcqRel) == PARKED {
			// SAFETY: waking a futex reads no user memory beyond the address.
			unsafe {
				libc::syscall(
					libc::SYS_futex,
					self.word.as_ptr(),
					libc::FUTEX_WAKE | libc::FUTEX_PRIVATE_FLAG,
					i32::MAX,
				);
			}
		}
	}

	/// The waker remote code holds; `wake` is [`Self::unpark`].
	pub(crate) fn waker(self: &Arc<Self>) -> Waker {
		Waker::from(self.clone())
	}
}

impl Wake for Unpark {
	fn wake(self: Arc<Self>) {
		self.unpark();
	}

	fn wake_by_ref(self: &Arc<Self>) {
		self.unpark();
	}
}
