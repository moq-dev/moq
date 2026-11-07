use crate::chunk::chunk_payload_data::ChunkPayloadData;

use alloc::collections::VecDeque;
use core::time::Duration;
use std::time::Instant;

/// pendingBaseQueue
pub(crate) type PendingBaseQueue = VecDeque<ChunkPayloadData>;

/// pendingQueue
#[derive(Debug, Default)]
pub(crate) struct PendingQueue {
    unordered_queue: PendingBaseQueue,
    ordered_queue: PendingBaseQueue,
    queue_len: usize,
    n_bytes: usize,
    selected: bool,
    unordered_is_selected: bool,
}

impl PendingQueue {
    pub(crate) fn new() -> Self {
        PendingQueue::default()
    }

    pub(crate) fn push(&mut self, c: ChunkPayloadData) {
        self.n_bytes += c.user_data.len();
        if c.unordered {
            self.unordered_queue.push_back(c);
        } else {
            self.ordered_queue.push_back(c);
        }
        self.queue_len += 1;
    }

    /// Start lifetimes for the newly queued suffix using the next transmit poll's time.
    /// Both queues retain message-ID order, so old pending messages need no rescan.
    pub(crate) fn start_timed_messages(&mut self, first_message_id: u64, now: Instant) {
        for queue in [&mut self.ordered_queue, &mut self.unordered_queue] {
            for chunk in queue
                .iter_mut()
                .rev()
                .take_while(|chunk| chunk.message_id >= first_message_id)
            {
                if chunk.deadline.is_some()
                    || chunk.payload_type == crate::PayloadProtocolIdentifier::Dcep
                {
                    continue;
                }
                if let Some((crate::ReliabilityType::Timed, lifetime)) = chunk.reliability {
                    chunk.deadline = now.checked_add(Duration::from_millis(u64::from(lifetime)));
                }
            }
        }
    }

    pub(crate) fn peek(&self) -> Option<&ChunkPayloadData> {
        if self.selected {
            if self.unordered_is_selected {
                return self.unordered_queue.front();
            } else {
                return self.ordered_queue.front();
            }
        }

        let c = self.unordered_queue.front();

        if c.is_some() {
            return c;
        }

        self.ordered_queue.front()
    }

    pub(crate) fn pop(
        &mut self,
        beginning_fragment: bool,
        unordered: bool,
    ) -> Option<ChunkPayloadData> {
        let popped = if self.selected {
            let popped = if self.unordered_is_selected {
                self.unordered_queue.pop_front()
            } else {
                self.ordered_queue.pop_front()
            };
            if let Some(p) = &popped {
                if p.ending_fragment {
                    self.selected = false;
                }
            }
            popped
        } else {
            if !beginning_fragment {
                return None;
            }
            if unordered {
                let popped = { self.unordered_queue.pop_front() };
                if let Some(p) = &popped {
                    if !p.ending_fragment {
                        self.selected = true;
                        self.unordered_is_selected = true;
                    }
                }
                popped
            } else {
                let popped = { self.ordered_queue.pop_front() };
                if let Some(p) = &popped {
                    if !p.ending_fragment {
                        self.selected = true;
                        self.unordered_is_selected = false;
                    }
                }
                popped
            }
        };

        if let Some(p) = &popped {
            self.n_bytes -= p.user_data.len();
            self.queue_len -= 1;
        }

        popped
    }

    /// Close the SSN gap after removing an entirely unsent ordered message.
    pub(crate) fn compact_ordered_ssns(&mut self, stream_identifier: u16) {
        for chunk in &mut self.ordered_queue {
            if chunk.stream_identifier == stream_identifier {
                chunk.stream_sequence_number = chunk.stream_sequence_number.wrapping_sub(1);
            }
        }
    }

    pub(crate) fn get_num_bytes(&self) -> usize {
        self.n_bytes
    }

    /// Remove a message without allocating a temporary collection. Selection
    /// persists when removing a different message from the pending queues.
    pub(crate) fn remove_message(
        &mut self,
        message_id: u64,
        mut on_remove: impl FnMut(ChunkPayloadData),
    ) -> usize {
        if self.selected && self.peek().is_some_and(|c| c.message_id == message_id) {
            self.selected = false;
        }
        let mut removed_bytes = 0;
        let mut removed_chunks = 0;
        for queue in [&mut self.ordered_queue, &mut self.unordered_queue] {
            queue.retain_mut(|chunk| {
                if chunk.message_id != message_id {
                    return true;
                }
                removed_bytes += chunk.user_data.len();
                removed_chunks += 1;
                on_remove(core::mem::take(chunk));
                false
            });
        }
        self.n_bytes -= removed_bytes;
        self.queue_len -= removed_chunks;
        removed_bytes
    }

    pub(crate) fn len(&self) -> usize {
        self.queue_len
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub(crate) fn contains_message(&self, message_id: u64) -> bool {
        self.unordered_queue
            .iter()
            .chain(self.ordered_queue.iter())
            .any(|chunk| chunk.message_id == message_id)
    }

    pub(crate) fn contains_stream(&self, stream_identifier: u16) -> bool {
        self.unordered_queue
            .iter()
            .chain(self.ordered_queue.iter())
            .any(|chunk| chunk.stream_identifier == stream_identifier)
    }
}
