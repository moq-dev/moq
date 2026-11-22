pub mod traits;

pub mod mmap;
pub mod userptr;

/// Whether a stream is running, and what userspace holds while it is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum State {
	/// Not streaming: every buffer is userspace's, none of them queued.
	Stopped,
	/// Streaming. `held` is whether userspace holds the buffer at
	/// `arena_index`, so the next frame queues it back first. A dequeue that
	/// timed out holds none.
	Streaming { held: bool },
}

/// A poll timeout in milliseconds, saturating at the longest one `poll` takes.
pub(crate) fn timeout_ms(duration: std::time::Duration) -> i32 {
	i32::try_from(duration.as_millis()).unwrap_or(i32::MAX)
}

#[cfg(test)]
mod tests {
	use std::time::Duration;

	use super::*;

	#[test]
	fn timeout_saturates() {
		assert_eq!(timeout_ms(Duration::from_millis(250)), 250);
		assert_eq!(timeout_ms(Duration::from_millis(i32::MAX as u64)), i32::MAX);
		assert_eq!(timeout_ms(Duration::from_millis(i32::MAX as u64 + 1)), i32::MAX);
		assert_eq!(timeout_ms(Duration::MAX), i32::MAX);
	}
}
