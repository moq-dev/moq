use std::{
    collections::{BTreeMap, VecDeque},
    mem,
    ops::Range,
    sync::{Arc, OnceLock, Weak},
};

use bytes::{Buf, Bytes};

use super::streams::ClosedStream;
use crate::{Instant, VarInt, range_set::RangeSet};

/// Where a [`Checkpoint`](super::Checkpoint) learns when the data before it was acknowledged
pub(super) type AckSlot = OnceLock<Result<Instant, ClosedStream>>;

/// Buffer of outgoing retransmittable stream data
#[derive(Default, Debug)]
pub(super) struct SendBuffer {
    /// Data queued by the application but not yet acknowledged. May or may not have been sent.
    unacked_segments: VecDeque<Bytes>,
    /// Total size of `unacked_segments`
    unacked_len: usize,
    /// Acknowledged bytes removed from the first segment's view but still held by its allocation
    front_trimmed: usize,
    /// The first offset that hasn't been written by the application, i.e. the offset past the end of `unacked`
    offset: u64,
    /// The first offset that hasn't been sent
    ///
    /// Always lies in (offset - unacked.len())..offset
    unsent: u64,
    /// Acknowledged ranges which couldn't be discarded yet as they don't include the earliest
    /// offset in `unacked`
    // TODO: Recover storage from these by compacting (#700)
    acks: RangeSet,
    /// When each range in `acks` was acknowledged, keyed by its start; the first ACK of a start wins
    ack_instants: BTreeMap<u64, Instant>,
    /// Previously transmitted ranges deemed lost
    retransmits: RangeSet,
    /// Checkpoints waiting for the acknowledged prefix to reach them
    watches: Watches,
    /// When the acknowledged prefix became complete: the latest instant among its ranges
    acked_at: Option<Instant>,
}

impl SendBuffer {
    /// Construct an empty buffer at the initial offset
    pub(super) fn new() -> Self {
        Self::default()
    }

    /// Append application data to the end of the stream
    pub(super) fn write(&mut self, data: Bytes) {
        self.unacked_len += data.len();
        self.offset += data.len() as u64;
        self.unacked_segments.push_back(data);
    }

    /// Discard a range of acknowledged stream data, acknowledged at `at`
    ///
    /// `at` is when the ACK was received, less the ACK delay the peer reported.
    pub(super) fn ack(&mut self, mut range: Range<u64>, at: Instant) {
        // Clamp the range to data which is still tracked, which `truncate` may have cut short
        let base_offset = self.offset - self.unacked_len as u64;
        range.start = base_offset.max(range.start);
        range.end = base_offset.max(range.end).min(self.offset);
        if range.is_empty() {
            // Nothing new, but a FIN-only ACK still reaches a checkpoint at the prefix
            let at = self.acked_at.map_or(at, |acked_at| acked_at.max(at));
            self.watches.ack(self.acked(), at);
            return;
        }
        self.acks.insert(range.clone());
        self.ack_instants.entry(range.start).or_insert(at);
        self.advance();
    }

    /// Discard the acknowledged ranges that extend the acknowledged prefix, resolving the
    /// checkpoints they reach
    ///
    /// The prefix up to an offset is complete only once every range below it is acknowledged,
    /// so a checkpoint takes the latest instant among them, even when one ACK covers ranges
    /// with different instants or a range waited behind a gap.
    fn advance(&mut self) {
        while self.acks.min() == Some(self.offset - self.unacked_len as u64) {
            let prefix = self.acks.pop_min().unwrap();
            let mut to_advance = (prefix.end - prefix.start) as usize;

            self.unacked_len -= to_advance;
            while to_advance > 0 {
                let front = self
                    .unacked_segments
                    .front_mut()
                    .expect("Expected buffered data");

                if front.len() <= to_advance {
                    to_advance -= front.len();
                    self.unacked_segments.pop_front();
                    self.front_trimmed = 0;

                    if self.unacked_segments.len() * 4 < self.unacked_segments.capacity() {
                        self.unacked_segments.shrink_to_fit();
                    }
                } else {
                    front.advance(to_advance);
                    self.front_trimmed += to_advance;
                    to_advance = 0;
                }
            }
        }

        let acked = self.acked();
        let held = self.ack_instants.split_off(&acked);
        let mut at = self.acked_at;
        for (start, instant) in mem::replace(&mut self.ack_instants, held) {
            if let Some(at) = at {
                self.watches.ack(start, at);
            }
            at = Some(at.map_or(instant, |at| at.max(instant)));
        }
        if let Some(at) = at {
            self.watches.ack(acked, at);
        }
        self.acked_at = at;
    }

    /// The end of the contiguous acknowledged prefix
    fn acked(&self) -> u64 {
        self.offset - self.unacked_len as u64
    }

    /// Watch for the peer to acknowledge everything written so far
    pub(super) fn watch(&mut self) -> Arc<AckSlot> {
        let slot = Arc::new(AckSlot::new());
        match self.acked_at {
            // Everything written is already acknowledged
            Some(at) if self.acked() == self.offset => {
                let _ = slot.set(Ok(at));
            }
            _ => self.watches.push(self.offset, &slot),
        }
        slot
    }

    /// Fail every checkpoint still waiting, since the peer stopped reading
    pub(super) fn abandon_watches(&mut self) {
        self.watches.fail_all();
    }

    /// Whether a checkpoint somebody still holds resolved since the last call
    pub(super) fn take_resolved(&mut self) -> bool {
        mem::take(&mut self.watches.resolved)
    }

    /// Compute the next range to transmit on this stream and update state to account for that
    /// transmission.
    ///
    /// `max_len` here includes the space which is available to transmit the
    /// offset and length of the data to send. The caller has to guarantee that
    /// there is at least enough space available to write maximum-sized metadata
    /// (8 byte offset + 8 byte length).
    ///
    /// The method returns a tuple:
    /// - The first return value indicates the range of data to send
    /// - The second return value indicates whether the length needs to be encoded
    ///   in the STREAM frames metadata (`true`), or whether it can be omitted
    ///   since the selected range will fill the whole packet.
    pub(super) fn poll_transmit(&mut self, mut max_len: usize) -> (Range<u64>, bool) {
        debug_assert!(max_len >= 8 + 8);
        let mut encode_length = false;

        if let Some(range) = self.retransmits.pop_min() {
            // Retransmit sent data

            // When the offset is known, we know how many bytes are required to encode it.
            // Offset 0 requires no space
            if range.start != 0 {
                max_len -= VarInt::size(unsafe { VarInt::from_u64_unchecked(range.start) });
            }
            if range.end - range.start < max_len as u64 {
                encode_length = true;
                max_len -= 8;
            }

            let end = range.end.min((max_len as u64).saturating_add(range.start));
            if end != range.end {
                self.retransmits.insert(end..range.end);
            }
            return (range.start..end, encode_length);
        }

        // Transmit new data

        // When the offset is known, we know how many bytes are required to encode it.
        // Offset 0 requires no space
        if self.unsent != 0 {
            max_len -= VarInt::size(unsafe { VarInt::from_u64_unchecked(self.unsent) });
        }
        if self.offset - self.unsent < max_len as u64 {
            encode_length = true;
            max_len -= 8;
        }

        let end = self
            .offset
            .min((max_len as u64).saturating_add(self.unsent));
        let result = self.unsent..end;
        self.unsent = end;
        (result, encode_length)
    }

    /// Returns data which is associated with a range
    ///
    /// This function can return a subset of the range, if the data is stored
    /// in noncontiguous fashion in the send buffer. In this case callers
    /// should call the function again with an incremented start offset to
    /// retrieve more data.
    pub(super) fn get(&self, offsets: Range<u64>) -> &[u8] {
        let base_offset = self.offset - self.unacked_len as u64;

        let mut segment_offset = base_offset;
        for segment in self.unacked_segments.iter() {
            if offsets.start >= segment_offset
                && offsets.start < segment_offset + segment.len() as u64
            {
                let start = (offsets.start - segment_offset) as usize;
                let end = (offsets.end - segment_offset) as usize;

                return &segment[start..end.min(segment.len())];
            }
            segment_offset += segment.len() as u64;
        }

        &[]
    }

    /// Abandon all data at or past `end`, which no longer needs to be sent or acknowledged
    ///
    /// The buffer then ends at `end`, so the caller tracks the stream's final size.
    pub(super) fn truncate(&mut self, end: u64) {
        self.watches.fail_past(end);
        self.cut(end);
        // Only a checkpoint taken before anything was acknowledged can still wait, and with
        // nothing left to acknowledge, no ACK will resolve it
        if self.is_fully_acked() {
            self.watches.fail_all();
        }
    }

    fn cut(&mut self, end: u64) {
        if end >= self.offset {
            return;
        }
        let base_offset = self.offset - self.unacked_len as u64;
        let mut keep = end.saturating_sub(base_offset) as usize;
        if keep == 0 {
            // Every byte before `end` was acknowledged, so nothing is retained
            *self = Self {
                offset: end,
                unsent: end,
                watches: mem::take(&mut self.watches),
                acked_at: self.acked_at,
                ..Self::default()
            };
            return;
        }
        let mut segments = 0;
        for segment in self.unacked_segments.iter_mut() {
            segments += 1;
            if segment.len() >= keep {
                segment.truncate(keep);
                break;
            }
            keep -= segment.len();
        }
        self.unacked_segments.truncate(segments);
        self.unacked_len = (end - base_offset) as usize;
        self.offset = end;
        self.unsent = self.unsent.min(end);
        self.acks.remove(end..u64::MAX);
        self.ack_instants.split_off(&end);
        self.retransmits.remove(end..u64::MAX);
    }

    /// Queue a range of sent but unacknowledged data to be retransmitted
    pub(super) fn retransmit(&mut self, mut range: Range<u64>) {
        // Data past a truncation is abandoned rather than lost
        range.end = range.end.min(self.offset);
        if range.is_empty() {
            return;
        }
        debug_assert!(range.end <= self.unsent, "unsent data can't be lost");
        self.retransmits.insert(range);
    }

    pub(super) fn retransmit_all_for_0rtt(&mut self) {
        debug_assert_eq!(self.offset, self.unacked_len as u64);
        self.unsent = 0;
    }

    /// First stream offset unwritten by the application, i.e. the offset that the next write will
    /// begin at
    pub(super) fn offset(&self) -> u64 {
        self.offset
    }

    /// Whether all sent data has been acknowledged
    pub(super) fn is_fully_acked(&self) -> bool {
        self.unacked_len == 0
    }

    /// Whether there's data to send
    ///
    /// There may be sent unacknowledged data even when this is false.
    pub(super) fn has_unsent_data(&self) -> bool {
        self.unsent != self.offset || !self.retransmits.is_empty()
    }

    /// Bytes still retained from application writes, including acknowledged data
    pub(super) fn buffered(&self) -> u64 {
        (self.unacked_len + self.front_trimmed) as u64
    }
}

/// Checkpoints waiting for the acknowledged prefix to reach their offset, in offset order
///
/// A dropped [`Checkpoint`](super::Checkpoint) leaves its entry behind until the prefix reaches
/// it, the next checkpoint is taken behind it, or the queue doubles past the live entries the
/// last sweep kept, so it holds at most about twice the live checkpoints. Dropping the stream
/// fails the rest, so no waiter hangs on a stream that is gone.
#[derive(Default, Debug)]
struct Watches {
    pending: VecDeque<(u64, Weak<AckSlot>)>,
    /// The length at which `push` next sweeps out dead entries
    sweep_at: usize,
    /// Whether a checkpoint somebody still holds resolved since the stream last reported it
    resolved: bool,
}

impl Watches {
    fn push(&mut self, offset: u64, slot: &Arc<AckSlot>) {
        // Checkpoints are taken at the write offset, which only grows
        debug_assert!(self.pending.back().is_none_or(|(last, _)| *last <= offset));
        // Each entry is popped at most once, so this stays amortized O(1)
        while self
            .pending
            .back()
            .is_some_and(|(_, slot)| slot.strong_count() == 0)
        {
            self.pending.pop_back();
        }
        // A checkpoint replaced by the next one dies after it is pushed, behind a live entry
        if self.pending.len() >= self.sweep_at {
            self.pending.retain(|(_, slot)| slot.strong_count() > 0);
            // Doubling keeps each sweep's cost covered by the pushes since the last one
            self.sweep_at = 2 * self.pending.len() + 1;
        }
        self.pending.push_back((offset, Arc::downgrade(slot)));
    }

    /// Resolve every checkpoint at or below the acknowledged prefix `acked`
    fn ack(&mut self, acked: u64, at: Instant) {
        while let Some((offset, _)) = self.pending.front()
            && *offset <= acked
        {
            let (_, slot) = self.pending.pop_front().unwrap();
            self.resolve(slot, Ok(at));
        }
    }

    /// Fail every checkpoint past `end`, which the peer will never acknowledge
    fn fail_past(&mut self, end: u64) {
        while let Some((offset, _)) = self.pending.back()
            && *offset > end
        {
            let (_, slot) = self.pending.pop_back().unwrap();
            self.resolve(slot, Err(ClosedStream::default()));
        }
    }

    fn fail_all(&mut self) {
        while let Some((_, slot)) = self.pending.pop_front() {
            self.resolve(slot, Err(ClosedStream::default()));
        }
    }

    fn resolve(&mut self, slot: Weak<AckSlot>, result: Result<Instant, ClosedStream>) {
        if let Some(slot) = slot.upgrade() {
            let _ = slot.set(result);
            self.resolved = true;
        }
    }
}

impl Drop for Watches {
    fn drop(&mut self) {
        self.fail_all();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Duration;

    #[test]
    fn fragment_with_length() {
        let mut buf = SendBuffer::new();
        const MSG: &[u8] = b"Hello, world!";
        buf.write(MSG.into());
        // 0 byte offset => 19 bytes left => 13 byte data isn't enough
        // with 8 bytes reserved for length 11 payload bytes will fit
        assert_eq!(buf.poll_transmit(19), (0..11, true));
        assert_eq!(
            buf.poll_transmit(MSG.len() + 16 - 11),
            (11..MSG.len() as u64, true)
        );
        assert_eq!(
            buf.poll_transmit(58),
            (MSG.len() as u64..MSG.len() as u64, true)
        );
    }

    #[test]
    fn fragment_without_length() {
        let mut buf = SendBuffer::new();
        const MSG: &[u8] = b"Hello, world with some extra data!";
        buf.write(MSG.into());
        // 0 byte offset => 19 bytes left => can be filled by 34 bytes payload
        assert_eq!(buf.poll_transmit(19), (0..19, false));
        assert_eq!(
            buf.poll_transmit(MSG.len() - 19 + 1),
            (19..MSG.len() as u64, false)
        );
        assert_eq!(
            buf.poll_transmit(58),
            (MSG.len() as u64..MSG.len() as u64, true)
        );
    }

    #[test]
    fn reserves_encoded_offset() {
        let mut buf = SendBuffer::new();

        // Pretend we have more than 1 GB of data in the buffer
        let chunk: Bytes = Bytes::from_static(&[0; 1024 * 1024]);
        for _ in 0..1025 {
            buf.write(chunk.clone());
        }

        const SIZE1: u64 = 64;
        const SIZE2: u64 = 16 * 1024;
        const SIZE3: u64 = 1024 * 1024 * 1024;

        // Offset 0 requires no space
        assert_eq!(buf.poll_transmit(16), (0..16, false));
        buf.retransmit(0..16);
        assert_eq!(buf.poll_transmit(16), (0..16, false));
        let mut transmitted = 16u64;

        // Offset 16 requires 1 byte
        assert_eq!(
            buf.poll_transmit((SIZE1 - transmitted + 1) as usize),
            (transmitted..SIZE1, false)
        );
        buf.retransmit(transmitted..SIZE1);
        assert_eq!(
            buf.poll_transmit((SIZE1 - transmitted + 1) as usize),
            (transmitted..SIZE1, false)
        );
        transmitted = SIZE1;

        // Offset 64 requires 2 bytes
        assert_eq!(
            buf.poll_transmit((SIZE2 - transmitted + 2) as usize),
            (transmitted..SIZE2, false)
        );
        buf.retransmit(transmitted..SIZE2);
        assert_eq!(
            buf.poll_transmit((SIZE2 - transmitted + 2) as usize),
            (transmitted..SIZE2, false)
        );
        transmitted = SIZE2;

        // Offset 16384 requires requires 4 bytes
        assert_eq!(
            buf.poll_transmit((SIZE3 - transmitted + 4) as usize),
            (transmitted..SIZE3, false)
        );
        buf.retransmit(transmitted..SIZE3);
        assert_eq!(
            buf.poll_transmit((SIZE3 - transmitted + 4) as usize),
            (transmitted..SIZE3, false)
        );
        transmitted = SIZE3;

        // Offset 1GB requires 8 bytes
        assert_eq!(
            buf.poll_transmit(chunk.len() + 8),
            (transmitted..transmitted + chunk.len() as u64, false)
        );
        buf.retransmit(transmitted..transmitted + chunk.len() as u64);
        assert_eq!(
            buf.poll_transmit(chunk.len() + 8),
            (transmitted..transmitted + chunk.len() as u64, false)
        );
    }

    #[test]
    fn multiple_segments() {
        let mut buf = SendBuffer::new();
        const MSG: &[u8] = b"Hello, world!";
        const MSG_LEN: u64 = MSG.len() as u64;

        const SEG1: &[u8] = b"He";
        buf.write(SEG1.into());
        const SEG2: &[u8] = b"llo,";
        buf.write(SEG2.into());
        const SEG3: &[u8] = b" w";
        buf.write(SEG3.into());
        const SEG4: &[u8] = b"o";
        buf.write(SEG4.into());
        const SEG5: &[u8] = b"rld!";
        buf.write(SEG5.into());

        assert_eq!(aggregate_unacked(&buf), MSG);

        assert_eq!(buf.poll_transmit(16), (0..8, true));
        assert_eq!(buf.get(0..5), SEG1);
        assert_eq!(buf.get(2..8), SEG2);
        assert_eq!(buf.get(6..8), SEG3);

        assert_eq!(buf.poll_transmit(16), (8..MSG_LEN, true));
        assert_eq!(buf.get(8..MSG_LEN), SEG4);
        assert_eq!(buf.get(9..MSG_LEN), SEG5);

        assert_eq!(buf.poll_transmit(42), (MSG_LEN..MSG_LEN, true));

        // Now drain the segments
        buf.ack(0..1, Instant::now());
        assert_eq!(aggregate_unacked(&buf), &MSG[1..]);
        buf.ack(0..3, Instant::now());
        assert_eq!(aggregate_unacked(&buf), &MSG[3..]);
        buf.ack(3..5, Instant::now());
        assert_eq!(aggregate_unacked(&buf), &MSG[5..]);
        buf.ack(7..9, Instant::now());
        assert_eq!(aggregate_unacked(&buf), &MSG[5..]);
        buf.ack(4..7, Instant::now());
        assert_eq!(aggregate_unacked(&buf), &MSG[9..]);
        buf.ack(0..MSG_LEN, Instant::now());
        assert_eq!(aggregate_unacked(&buf), &[] as &[u8]);
    }

    #[test]
    fn retransmit() {
        let mut buf = SendBuffer::new();
        const MSG: &[u8] = b"Hello, world with extra data!";
        buf.write(MSG.into());
        // Transmit two frames
        assert_eq!(buf.poll_transmit(16), (0..16, false));
        assert_eq!(buf.poll_transmit(16), (16..23, true));
        // Lose the first, but not the second
        buf.retransmit(0..16);
        // Ensure we only retransmit the lost frame, then continue sending fresh data
        assert_eq!(buf.poll_transmit(16), (0..16, false));
        assert_eq!(buf.poll_transmit(16), (23..MSG.len() as u64, true));
        // Lose the second frame
        buf.retransmit(16..23);
        assert_eq!(buf.poll_transmit(16), (16..23, true));
    }

    #[test]
    fn ack() {
        let mut buf = SendBuffer::new();
        const MSG: &[u8] = b"Hello, world!";
        buf.write(MSG.into());
        assert_eq!(buf.poll_transmit(16), (0..8, true));
        buf.ack(0..8, Instant::now());
        assert_eq!(aggregate_unacked(&buf), &MSG[8..]);
    }

    #[test]
    fn reordered_ack() {
        let mut buf = SendBuffer::new();
        const MSG: &[u8] = b"Hello, world with extra data!";
        buf.write(MSG.into());
        assert_eq!(buf.poll_transmit(16), (0..16, false));
        assert_eq!(buf.poll_transmit(16), (16..23, true));
        buf.ack(16..23, Instant::now());
        assert_eq!(aggregate_unacked(&buf), MSG);
        buf.ack(0..16, Instant::now());
        assert_eq!(aggregate_unacked(&buf), &MSG[23..]);
        assert!(buf.acks.is_empty());
    }

    #[test]
    fn truncate_keeps_prefix() {
        let mut buf = SendBuffer::new();
        buf.write(Bytes::from_static(b"Hello, "));
        buf.write(Bytes::from_static(b"world!"));
        assert_eq!(buf.poll_transmit(16), (0..8, true));
        buf.ack(0..2, Instant::now());
        buf.retransmit(2..8);

        buf.truncate(9);
        assert_eq!(aggregate_unacked(&buf), b"llo, wo");
        assert_eq!(buf.offset(), 9);
        // The lost range is retransmitted, then the unsent byte before the truncation
        assert_eq!(buf.poll_transmit(16), (2..8, true));
        assert_eq!(buf.poll_transmit(16), (8..9, true));
        assert!(!buf.has_unsent_data());

        // Late acks and losses past the truncation are ignored
        buf.ack(9..13, Instant::now());
        buf.retransmit(9..13);
        assert!(!buf.has_unsent_data());
        buf.ack(2..9, Instant::now());
        assert!(buf.is_fully_acked());
        assert_eq!(buf.buffered(), 0);
    }

    #[test]
    fn truncate_acked_prefix() {
        let mut buf = SendBuffer::new();
        buf.write(Bytes::from_static(b"Hello, world!"));
        assert_eq!(buf.poll_transmit(16), (0..8, true));
        buf.ack(0..8, Instant::now());
        buf.truncate(4);
        assert!(buf.is_fully_acked());
        assert!(!buf.has_unsent_data());
        assert_eq!(buf.buffered(), 0);
    }

    #[test]
    fn watch_waits_for_prefix() {
        let mut buf = SendBuffer::new();
        buf.write(Bytes::from_static(b"Hello, "));
        let first = buf.watch();
        buf.write(Bytes::from_static(b"world!"));
        let second = buf.watch();
        assert_eq!(buf.poll_transmit(16), (0..8, true));
        assert_eq!(buf.poll_transmit(16), (8..13, true));

        // Acknowledged past both, but not contiguously
        let early = Instant::now();
        buf.ack(8..13, early);
        assert_eq!(first.get(), None);
        assert_eq!(second.get(), None);
        assert!(!buf.take_resolved());

        // Filling the gap in the middle of the first segment resolves both, with that ACK
        let late = early + Duration::from_millis(1);
        buf.ack(0..8, late);
        assert_eq!(first.get(), Some(&Ok(late)));
        assert_eq!(second.get(), Some(&Ok(late)));
        assert!(buf.take_resolved());
        assert!(!buf.take_resolved());

        // Everything written is acknowledged, by the ACK that got there
        assert_eq!(buf.watch().get(), Some(&Ok(late)));
    }

    #[test]
    fn watch_takes_the_latest_instant_below_it() {
        let mut buf = SendBuffer::new();
        buf.write(Bytes::from_static(b"Hello, "));
        let first = buf.watch();
        buf.write(Bytes::from_static(b"world!"));
        let second = buf.watch();
        assert_eq!(buf.poll_transmit(16), (0..8, true));
        assert_eq!(buf.poll_transmit(16), (8..13, true));

        // One ACK, in packet order: the held range came in a packet before the largest, so it
        // keeps the receive time, while the largest fills the gap less its ACK delay
        let early = Instant::now();
        let late = early + Duration::from_millis(1);
        buf.ack(8..13, late);
        buf.ack(0..8, early);
        assert_eq!(first.get(), Some(&Ok(early)));
        assert_eq!(second.get(), Some(&Ok(late)));

        // A later range acknowledged at an earlier instant can't complete the prefix before it
        buf.write(Bytes::from_static(b"!"));
        let third = buf.watch();
        assert_eq!(buf.poll_transmit(16), (13..14, true));
        buf.ack(13..14, early);
        assert_eq!(third.get(), Some(&Ok(late)));
    }

    #[test]
    fn watch_fails_past_truncation() {
        let mut buf = SendBuffer::new();
        buf.write(Bytes::from_static(b"Hello, "));
        let kept = buf.watch();
        buf.write(Bytes::from_static(b"world!"));
        let dropped = buf.watch();

        buf.truncate(7);
        assert_eq!(dropped.get(), Some(&Err(ClosedStream::default())));
        assert_eq!(kept.get(), None);
        assert!(buf.take_resolved());

        // A stream that goes away fails what it never acknowledged
        drop(buf);
        assert_eq!(kept.get(), Some(&Err(ClosedStream::default())));
    }

    /// A checkpoint on an empty stream fails, and is reported, once a reset leaves nothing to
    /// acknowledge.
    #[test]
    fn watch_fails_on_empty_reset() {
        let mut buf = SendBuffer::new();
        let empty = buf.watch();
        buf.truncate(0);
        assert_eq!(empty.get(), Some(&Err(ClosedStream::default())));
        assert!(buf.take_resolved());
    }

    /// Checkpoints taken and dropped on a stream whose prefix never moves do not pile up.
    #[test]
    fn dropped_watches_are_pruned() {
        let mut buf = SendBuffer::new();
        buf.write(Bytes::from_static(b"Hello"));
        let held = buf.watch();
        for _ in 0..1000 {
            drop(buf.watch());
        }
        assert!(
            buf.watches.pending.len() <= 2,
            "{}",
            buf.watches.pending.len()
        );

        // Nothing was ever written or acknowledged on this one
        let mut empty = SendBuffer::new();
        for _ in 0..1000 {
            drop(empty.watch());
        }
        assert!(empty.watches.pending.len() <= 1);

        assert_eq!(buf.poll_transmit(16), (0..5, true));
        buf.ack(0..5, Instant::now());
        assert!(held.get().is_some());
        assert!(buf.watches.pending.is_empty());
    }

    #[test]
    fn replaced_watches_are_pruned() {
        // `last = stream.checkpoint()` takes the new checkpoint before dropping the old one, so
        // each dead entry sits behind a live one while nothing is acknowledged
        let mut buf = SendBuffer::new();
        let mut last = buf.watch();
        for _ in 0..1000 {
            buf.write(Bytes::from_static(b"x"));
            last = buf.watch();
        }
        assert!(
            buf.watches.pending.len() <= 3,
            "{}",
            buf.watches.pending.len()
        );

        assert_eq!(buf.poll_transmit(2000), (0..1000, true));
        buf.ack(0..1000, Instant::now());
        assert!(last.get().is_some());
    }

    #[test]
    fn dropped_watch_is_not_reported() {
        let mut buf = SendBuffer::new();
        buf.write(Bytes::from_static(b"Hello"));
        drop(buf.watch());
        assert_eq!(buf.poll_transmit(16), (0..5, true));
        buf.ack(0..5, Instant::now());
        assert!(!buf.take_resolved());
    }

    fn aggregate_unacked(buf: &SendBuffer) -> Vec<u8> {
        let mut result = Vec::new();
        for segment in buf.unacked_segments.iter() {
            result.extend_from_slice(&segment[..]);
        }
        result
    }
}
