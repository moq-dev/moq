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
