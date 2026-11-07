use std::time::{Duration, Instant};

use crate::rtp_::MidRid;
use crate::util::not_happening;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QueueSnapshot {
    /// Time this snapshot was made
    pub created_at: Instant,
    /// The total byte size of the snapshot.
    pub byte_size: usize,
    /// The total number of packets in the queue.
    /// NB: This is not a [`usize`] because it will later be used to divide a [`Duration`], for which
    /// [`usize`] isn't implement. If the queues end up with 2^32 packets something has gone very wrong
    /// in any case.
    pub packet_count: u32,
    /// Accumulation of all queue time at the time point `created_at`. To use this
    /// Look at `total_queue_time(now)` which allows getting the queue time at a later Instant.
    pub total_queue_time_origin: Duration,
    /// Last time something was emitted from this queue.
    pub last_emitted: Option<Instant>,
    /// Time the first unsent packet has spent in the queue.
    pub first_unsent: Option<Instant>,
    /// The priority of the most important packet in the queue.
    pub priority: QueuePriority,
}

/// Priority for a given queue.
///
/// When sorted, higher priority sorts first.
#[derive(Default, Debug, Copy, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum QueuePriority {
    // Highest, priority for a queue that contains media.
    Media = 0,
    // Priority for a queue that only contains padding.
    Padding = 1,
    // Priority for an empty queue.
    #[default]
    Empty = 2,
}

impl QueueSnapshot {
    /// Update the priority if the snapshot is non-empty.
    ///
    /// Sets the priority to the provided priority if the queue is non-empty, otherwise empty.
    pub fn update_priority(&mut self, priority: QueuePriority) {
        if self.packet_count > 0 {
            self.priority = priority;
        } else {
            self.priority = QueuePriority::Empty;
        }
    }
}

impl Default for QueueSnapshot {
    fn default() -> Self {
        Self {
            created_at: not_happening(),
            byte_size: Default::default(),
            packet_count: Default::default(),
            total_queue_time_origin: Default::default(),
            last_emitted: Default::default(),
            first_unsent: Default::default(),
            priority: QueuePriority::default(),
        }
    }
}

/// The state of a single upstream queue.
/// The pacer manages packets across several upstream queues.
#[derive(Debug, Clone, Copy)]
pub struct QueueState {
    pub midrid: MidRid,
    pub unpaced: bool,
    pub use_for_padding: bool,
    pub snapshot: QueueSnapshot,
}

/// A request to generate a specific amount of padding.
#[derive(Debug, Clone, Copy)]
pub struct PaddingRequest {
    /// The Mid that should generate and queue the padding.
    pub midrid: MidRid,
    /// The amount of padding in bytes to generate.
    pub padding: usize,
}

impl QueueSnapshot {
    /// Merge other into self.
    pub fn merge(&mut self, other: &Self) {
        self.created_at = self.created_at.min(other.created_at);
        self.byte_size += other.byte_size;
        self.packet_count += other.packet_count;
        self.total_queue_time_origin += other.total_queue_time_origin;
        self.last_emitted = self.last_emitted.max(other.last_emitted);
        self.first_unsent = match (self.first_unsent, other.first_unsent) {
            (None, None) => None,
            (None, Some(v2)) => Some(v2),
            (Some(v1), None) => Some(v1),
            (Some(v1), Some(v2)) => Some(v1.min(v2)),
        };
        self.priority = self.priority.min(other.priority);
    }

    pub fn total_queue_time(&self, now: Instant) -> Duration {
        self.total_queue_time_origin + self.packet_count * (now - self.created_at)
    }
}
