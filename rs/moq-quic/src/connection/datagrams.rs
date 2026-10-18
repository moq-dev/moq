use std::collections::VecDeque;

use bytes::Bytes;
use thiserror::Error;
use tracing::{debug, trace};

use super::{Connection, SendGroup};
use crate::{
    TransportError,
    frame::{Datagram, FrameStruct},
};

/// API to control datagram traffic
pub struct Datagrams<'a> {
    pub(super) conn: &'a mut Connection,
}

impl Datagrams<'_> {
    /// Queue an unreliable, unordered datagram in `group` at `order`
    ///
    /// The datagram is scheduled like one of the group's streams at priority `order`: it waits
    /// behind higher orders, goes ahead of a stream of equal order, and spends the group's
    /// share of bytes. Without a group, it is ordered among the ungrouped streams, which go
    /// before every group.
    ///
    /// Sending never blocks. While queued datagrams overflow
    /// [`TransportConfig::datagram_send_buffer_size`](crate::TransportConfig::datagram_send_buffer_size),
    /// the domain holding the most datagram bytes drops the oldest datagram of its
    /// lowest-priority group, and this datagram is dropped instead once it is the one to go.
    ///
    /// Returns `Err` iff a `len`-byte datagram cannot currently be sent.
    ///
    /// # Panics
    /// - when `group` was opened on another connection
    pub fn send(
        &mut self,
        data: Bytes,
        group: Option<&SendGroup>,
        order: i64,
    ) -> Result<(), SendDatagramError> {
        if self.conn.config.datagram_receive_buffer_size.is_none() {
            return Err(SendDatagramError::Disabled);
        }
        let max = self
            .max_size()
            .ok_or(SendDatagramError::UnsupportedByPeer)?;
        let budget = self.conn.config.datagram_send_buffer_size;
        let datagram = Datagram { data };
        if datagram.data.len() > max || datagram.size(true) > budget {
            return Err(SendDatagramError::TooLarge);
        }
        if !self
            .conn
            .streams
            .queue_datagram(datagram, group, order, budget)
        {
            trace!("dropping outgoing datagram");
        }
        Ok(())
    }

    /// Discard queued datagrams that no longer fit the active path.
    pub(super) fn drop_oversized(&mut self) {
        let Some(max_datagram_size) = self.max_size() else {
            return;
        };
        self.conn
            .streams
            .drop_oversized_datagrams(max_datagram_size);
    }

    /// Compute the maximum size of datagrams that may passed to `send_datagram`
    ///
    /// Returns `None` if datagrams are unsupported by the peer or disabled locally.
    ///
    /// This may change over the lifetime of a connection according to variation in the path MTU
    /// estimate. The peer can also enforce an arbitrarily small fixed limit, but if the peer's
    /// limit is large this is guaranteed to be a little over a kilobyte at minimum.
    ///
    /// Not necessarily the maximum size of received datagrams.
    pub fn max_size(&self) -> Option<usize> {
        // We use the conservative overhead bound for any packet number, reducing the budget by at
        // most 3 bytes, so that PN size fluctuations don't cause users sending maximum-size
        // datagrams to suffer avoidable packet loss.
        let max_size = self.conn.path.current_mtu() as usize
            - self.conn.predict_1rtt_overhead(None)
            - Datagram::SIZE_BOUND;
        let limit = self
            .conn
            .peer_params
            .max_datagram_frame_size?
            .into_inner()
            .saturating_sub(Datagram::SIZE_BOUND as u64);
        Some(limit.min(max_size as u64) as usize)
    }

    /// Receive an unreliable, unordered datagram
    pub fn recv(&mut self) -> Option<Bytes> {
        self.conn.datagrams.recv()
    }

    /// Bytes available in the outgoing datagram buffer
    ///
    /// When greater than zero, [`send`](Self::send)ing a datagram of at most this size is
    /// guaranteed not to cause queued datagrams to be dropped.
    pub fn send_buffer_space(&self) -> usize {
        self.conn
            .config
            .datagram_send_buffer_size
            .saturating_sub(self.conn.streams.datagram_bytes())
            .saturating_sub(Datagram::SIZE_BOUND)
    }
}

/// Incoming datagrams; outgoing ones are scheduled with the streams.
#[derive(Default)]
pub(super) struct DatagramState {
    pub(super) incoming: DatagramBuffer,
}

impl DatagramState {
    pub(super) fn received(
        &mut self,
        datagram: Datagram,
        window: &Option<usize>,
    ) -> Result<bool, TransportError> {
        let window = match window {
            None => {
                return Err(TransportError::PROTOCOL_VIOLATION(
                    "unexpected DATAGRAM frame",
                ));
            }
            Some(x) => *x,
        };

        let size_with_overhead = datagram.data.len() + size_of::<Datagram>();

        if size_with_overhead > window {
            return Err(TransportError::PROTOCOL_VIOLATION("oversized datagram"));
        }

        let was_empty = self.incoming.is_empty();
        while self.incoming.memory_used() + size_with_overhead > window {
            debug!("dropping stale datagram");
            self.recv();
        }

        self.incoming.push_back(datagram);
        Ok(was_empty)
    }

    pub(super) fn recv(&mut self) -> Option<Bytes> {
        let x = self.incoming.pop_front()?.data;
        Some(x)
    }
}

#[derive(Default)]
pub(super) struct DatagramBuffer {
    queue: VecDeque<Datagram>,
    payload_bytes: usize,
}

impl DatagramBuffer {
    fn push_back(&mut self, datagram: Datagram) {
        self.payload_bytes += datagram.data.len();
        self.queue.push_back(datagram);
    }

    fn pop_front(&mut self) -> Option<Datagram> {
        let datagram = self.queue.pop_front()?;
        self.payload_bytes -= datagram.data.len();
        Some(datagram)
    }

    fn memory_used(&self) -> usize {
        self.payload_bytes
            .saturating_add(self.queue.len() * size_of::<Datagram>())
    }

    pub(super) fn is_empty(&self) -> bool {
        self.queue.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_frame_flood_limit() {
        let mut state = DatagramState::default();
        let datagram = Datagram { data: Bytes::new() };
        let window = 100;
        loop {
            let initial_count = state.incoming.queue.len();
            state.received(datagram.clone(), &Some(window)).unwrap();
            assert!(state.incoming.queue.len() * size_of::<Datagram>() <= window);
            if state.incoming.queue.len() == initial_count {
                // Datagrams are getting dropped
                break;
            }
        }
    }
}

/// Errors that can arise when sending a datagram
#[derive(Debug, Error, Clone, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub enum SendDatagramError {
    /// The peer does not support receiving datagrams
    #[error("datagrams not supported by peer")]
    UnsupportedByPeer,
    /// Datagram support is disabled locally
    #[error("datagram support disabled")]
    Disabled,
    /// The datagram is larger than the connection can currently accommodate
    ///
    /// Indicates that the path MTU minus overhead or the limit advertised by the peer has been
    /// exceeded.
    #[error("datagram too large")]
    TooLarge,
}
