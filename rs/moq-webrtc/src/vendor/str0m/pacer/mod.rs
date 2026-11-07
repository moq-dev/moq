mod null;
pub(crate) use null::NullPacer;

mod queue;
pub(crate) use queue::{QueuePriority, QueueSnapshot, QueueState};

/// The sub-reason for the [`Reason::Pacer`][crate::vendor::str0m::Reason::Pacer].
///
/// This enum is not considered stable API and may change in minor revisions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum PacerReason {
	/// Handle to update the pacer's queue snapshots.
	Handle,
}
