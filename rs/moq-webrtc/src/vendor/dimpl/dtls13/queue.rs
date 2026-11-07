//! Queue wrapper types with safe Debug implementations.
//!
//! These wrappers ensure that debug output only shows metadata,
//! not potentially sensitive payload data.

use std::collections::VecDeque;
use std::fmt;
use std::ops::{Deref, DerefMut};

use crate::buffer::Buf;
use crate::dtls13::incoming::Incoming;
use crate::types::ContentType;

/// Wrapper around the receive queue that provides safe Debug output.
///
/// The Debug implementation only shows metadata (counts by content type),
/// not potentially sensitive payload data.
pub(crate) struct QueueRx(VecDeque<Incoming>);

impl QueueRx {
    pub fn new() -> Self {
        Self(VecDeque::new())
    }
}

impl Deref for QueueRx {
    type Target = VecDeque<Incoming>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl DerefMut for QueueRx {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl fmt::Debug for QueueRx {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut handshake = 0;
        let mut app_data = 0;
        let mut alert = 0;
        let mut other = 0;
        let mut min_seq: Option<(u16, u64)> = None;
        let mut max_seq: Option<(u16, u64)> = None;

        for item in &self.0 {
            let record = item.first().record();
            match record.content_type {
                ContentType::Handshake => handshake += 1,
                ContentType::ApplicationData => app_data += 1,
                ContentType::Alert => alert += 1,
                _ => other += 1,
            }

            let seq = (record.sequence.epoch, record.sequence.sequence_number);
            min_seq = Some(min_seq.map_or(seq, |m| m.min(seq)));
            max_seq = Some(max_seq.map_or(seq, |m| m.max(seq)));
        }

        let mut s = f.debug_struct("QueueRx");
        s.field("len", &self.0.len())
            .field("handshake", &handshake)
            .field("app_data", &app_data)
            .field("alert", &alert)
            .field("other", &other);

        if let (Some(min), Some(max)) = (min_seq, max_seq) {
            s.field(
                "seq_range",
                &format_args!("{}:{} - {}:{}", min.0, min.1, max.0, max.1),
            );
        }

        s.finish()
    }
}

/// Wrapper around the transmit queue that provides safe Debug output.
///
/// The Debug implementation only shows metadata (datagram count and total bytes),
/// not potentially sensitive payload data.
pub(crate) struct QueueTx(VecDeque<Buf>);

impl QueueTx {
    pub fn new() -> Self {
        Self(VecDeque::new())
    }
}

impl Deref for QueueTx {
    type Target = VecDeque<Buf>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl DerefMut for QueueTx {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl fmt::Debug for QueueTx {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let total_bytes: usize = self.0.iter().map(|b| b.len()).sum();
        f.debug_struct("QueueTx")
            .field("datagrams", &self.0.len())
            .field("total_bytes", &total_bytes)
            .finish()
    }
}
