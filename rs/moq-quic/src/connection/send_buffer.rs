use std::{collections::VecDeque, ops::Range};

use bytes::{Buf, Bytes};

use crate::{VarInt, range_set::RangeSet};

/// Buffer of outgoing retransmittable stream data
#[derive(Default, Debug)]
pub(super) struct SendBuffer {
    /// Data queued by the application but not yet acknowledged. May or may not have been sent.
    unacked_segments: VecDeque<Segment>,
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
    /// Previously transmitted ranges deemed lost
    retransmits: RangeSet,
}

/// The end of the part of `range` a STREAM frame carries in `max_len` bytes, and whether the
/// frame encodes its length, which it omits when the data fills the rest of the packet
///
/// `max_len` includes the space for the frame's offset and length. The caller has to guarantee
/// that there is at least enough space available to write maximum-sized metadata (8 byte offset +
/// 8 byte length).
fn fit(range: Range<u64>, mut max_len: usize) -> (u64, bool) {
    debug_assert!(max_len >= 8 + 8);
    let mut encode_length = false;
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
    (end, encode_length)
}

/// One application write, still retained
#[derive(Debug)]
struct Segment {
    /// The stream offset of the first byte of `data`
    offset: u64,
    data: Bytes,
}

impl SendBuffer {
    /// Construct an empty buffer at the initial offset
    pub(super) fn new() -> Self {
        Self::default()
    }

    /// Append application data to the end of the stream
    pub(super) fn write(&mut self, data: Bytes) {
        let offset = self.offset;
        self.unacked_len += data.len();
        self.offset += data.len() as u64;
        self.unacked_segments.push_back(Segment { offset, data });
    }

    /// Discard a range of acknowledged stream data
    pub(super) fn ack(&mut self, mut range: Range<u64>) {
        // Clamp the range to data which is still tracked, which `truncate` may have cut short
        let base_offset = self.offset - self.unacked_len as u64;
        range.start = base_offset.max(range.start);
        range.end = base_offset.max(range.end).min(self.offset);
        if range.is_empty() {
            return;
        }

        self.acks.insert(range);

        while self.acks.min() == Some(self.offset - self.unacked_len as u64) {
            let prefix = self.acks.pop_min().unwrap();
            let mut to_advance = (prefix.end - prefix.start) as usize;

            self.unacked_len -= to_advance;
            while to_advance > 0 {
                let front = self
                    .unacked_segments
                    .front_mut()
                    .expect("Expected buffered data");

                if front.data.len() <= to_advance {
                    to_advance -= front.data.len();
                    self.unacked_segments.pop_front();
                    self.front_trimmed = 0;

                    if self.unacked_segments.len() * 4 < self.unacked_segments.capacity() {
                        self.unacked_segments.shrink_to_fit();
                    }
                } else {
                    front.data.advance(to_advance);
                    front.offset += to_advance as u64;
                    self.front_trimmed += to_advance;
                    to_advance = 0;
                }
            }
        }
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
    pub(super) fn poll_transmit(&mut self, max_len: usize) -> (Range<u64>, bool) {
        if let Some(range) = self.retransmits.pop_min() {
            // Retransmit sent data
            let (end, encode_length) = fit(range.clone(), max_len);
            if end != range.end {
                self.retransmits.insert(end..range.end);
            }
            return (range.start..end, encode_length);
        }

        // Transmit new data
        let (end, encode_length) = fit(self.unsent..self.offset, max_len);
        let result = self.unsent..end;
        self.unsent = end;
        (result, encode_length)
    }

    /// Whether part of sent `range` lies past the acknowledged prefix
    pub(super) fn unacked(&self, range: Range<u64>) -> bool {
        range.start.max(self.acked()) < range.end.min(self.offset)
    }

    /// The part of sent `range` past the acknowledged prefix, sized like [`Self::poll_transmit`]
    /// to send again without queueing a retransmission, or `None` if none of it is left
    pub(super) fn duplicate(&self, range: Range<u64>, max_len: usize) -> Option<(Range<u64>, bool)> {
        let start = range.start.max(self.acked());
        // Truncation abandons data past it
        let end = range.end.min(self.offset);
        if start >= end {
            return None;
        }
        let (end, encode_length) = fit(start..end, max_len);
        Some((start..end, encode_length))
    }

    /// Returns the data in a range of unacknowledged offsets, one slice per write it spans
    ///
    /// A binary search finds the first write, so a packet's cost does not grow with every write
    /// retained ahead of it.
    pub(super) fn get(&self, offsets: Range<u64>) -> impl Iterator<Item = &[u8]> {
        assert!(
            self.acked() <= offsets.start && offsets.end <= self.offset,
            "{offsets:?} is not retained"
        );
        // The write holding the first byte is the last one starting at or before it
        let first = self
            .unacked_segments
            .partition_point(|segment| segment.offset <= offsets.start)
            .saturating_sub(1);
        self.unacked_segments
            .range(first..)
            .take_while(move |segment| segment.offset < offsets.end)
            .map(move |segment| {
                let start = offsets.start.saturating_sub(segment.offset) as usize;
                let end = (offsets.end - segment.offset).min(segment.data.len() as u64) as usize;
                &segment.data[start..end]
            })
    }

    /// Abandon all data at or past `end`, which no longer needs to be sent or acknowledged
    ///
    /// The buffer then ends at `end`, so the caller tracks the stream's final size.
    pub(super) fn truncate(&mut self, end: u64) {
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
                ..Self::default()
            };
            return;
        }
        let mut segments = 0;
        for segment in self.unacked_segments.iter_mut() {
            segments += 1;
            if segment.data.len() >= keep {
                segment.data.truncate(keep);
                break;
            }
            keep -= segment.data.len();
        }
        self.unacked_segments.truncate(segments);
        self.unacked_len = (end - base_offset) as usize;
        self.offset = end;
        self.unsent = self.unsent.min(end);
        self.acks.remove(end..u64::MAX);
        self.retransmits.remove(end..u64::MAX);
    }

    /// Queue a range of sent data to be retransmitted, skipping whatever was acknowledged
    ///
    /// A bandwidth probe's duplicate can deliver the data before its original is declared lost.
    pub(super) fn retransmit(&mut self, mut range: Range<u64>) {
        // Data past a truncation is abandoned rather than lost
        range.start = range.start.max(self.acked());
        range.end = range.end.min(self.offset);
        if range.is_empty() {
            return;
        }
        debug_assert!(range.end <= self.unsent, "unsent data can't be lost");
        self.retransmits.insert(range.clone());
        for acked in self.acks.iter() {
            if acked.start >= range.end {
                break;
            }
            self.retransmits.remove(acked);
        }
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

    /// The end of the contiguous acknowledged prefix
    pub(super) fn acked(&self) -> u64 {
        self.offset - self.unacked_len as u64
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

#[cfg(test)]
mod tests {
    use super::*;

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
        assert_eq!(get(&buf, 0..8), [SEG1, SEG2, SEG3]);
        assert_eq!(get(&buf, 3..7), [&SEG2[1..], &SEG3[..1]]);

        assert_eq!(buf.poll_transmit(16), (8..MSG_LEN, true));
        assert_eq!(get(&buf, 8..MSG_LEN), [SEG4, SEG5]);
        assert_eq!(get(&buf, 10..12), [&SEG5[1..3]]);

        // A FIN alone carries no data
        assert_eq!(buf.poll_transmit(42), (MSG_LEN..MSG_LEN, true));
        assert!(get(&buf, MSG_LEN..MSG_LEN).concat().is_empty());

        // Now drain the segments, which moves where the retained data starts
        buf.ack(0..1);
        assert_eq!(aggregate_unacked(&buf), &MSG[1..]);
        assert_eq!(get(&buf, 1..3), [&SEG1[1..], &SEG2[..1]]);
        buf.ack(0..3);
        assert_eq!(aggregate_unacked(&buf), &MSG[3..]);
        assert_eq!(get(&buf, 3..7), [&SEG2[1..], &SEG3[..1]]);
        buf.ack(3..5);
        assert_eq!(aggregate_unacked(&buf), &MSG[5..]);
        buf.ack(7..9);
        assert_eq!(aggregate_unacked(&buf), &MSG[5..]);
        // Acknowledged data past a hole is still retained, and still found by offset
        assert_eq!(get(&buf, 5..10), [&SEG2[3..], SEG3, SEG4, &SEG5[..1]]);
        buf.ack(4..7);
        assert_eq!(aggregate_unacked(&buf), &MSG[9..]);
        buf.ack(0..MSG_LEN);
        assert_eq!(aggregate_unacked(&buf), &[] as &[u8]);
        assert!(get(&buf, MSG_LEN..MSG_LEN).concat().is_empty());
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

    /// A duplicate can deliver data before its original is declared lost, so a loss resends
    /// only what no copy delivered
    #[test]
    fn retransmit_skips_acked() {
        let mut buf = SendBuffer::new();
        const MSG: &[u8] = b"Hello, world with extra data!";
        buf.write(MSG.into());
        assert_eq!(buf.poll_transmit(64), (0..29, true));
        buf.ack(0..4);
        buf.ack(10..20);
        buf.retransmit(0..29);
        assert_eq!(buf.poll_transmit(64), (4..10, true));
        assert_eq!(buf.poll_transmit(64), (20..29, true));
        assert_eq!(buf.poll_transmit(64), (29..29, true));
    }

    #[test]
    fn duplicate() {
        let mut buf = SendBuffer::new();
        const MSG: &[u8] = b"Hello, world with extra data!";
        buf.write(MSG.into());
        assert_eq!(buf.poll_transmit(64), (0..29, true));
        buf.ack(0..4);
        // Starts past the acknowledged prefix and leaves nothing queued to retransmit
        assert_eq!(buf.duplicate(0..29, 16), Some((4..19, false)));
        assert_eq!(buf.poll_transmit(64), (29..29, true));
        // Nothing past a truncation or the acknowledged prefix is sent again
        buf.truncate(20);
        assert_eq!(buf.duplicate(16..29, 64), Some((16..20, true)));
        assert_eq!(buf.duplicate(0..4, 64), None);
    }

    #[test]
    fn ack() {
        let mut buf = SendBuffer::new();
        const MSG: &[u8] = b"Hello, world!";
        buf.write(MSG.into());
        assert_eq!(buf.poll_transmit(16), (0..8, true));
        buf.ack(0..8);
        assert_eq!(aggregate_unacked(&buf), &MSG[8..]);
    }

    #[test]
    fn reordered_ack() {
        let mut buf = SendBuffer::new();
        const MSG: &[u8] = b"Hello, world with extra data!";
        buf.write(MSG.into());
        assert_eq!(buf.poll_transmit(16), (0..16, false));
        assert_eq!(buf.poll_transmit(16), (16..23, true));
        buf.ack(16..23);
        assert_eq!(aggregate_unacked(&buf), MSG);
        buf.ack(0..16);
        assert_eq!(aggregate_unacked(&buf), &MSG[23..]);
        assert!(buf.acks.is_empty());
    }

    #[test]
    fn truncate_keeps_prefix() {
        let mut buf = SendBuffer::new();
        buf.write(Bytes::from_static(b"Hello, "));
        buf.write(Bytes::from_static(b"world!"));
        assert_eq!(buf.poll_transmit(16), (0..8, true));
        buf.ack(0..2);
        buf.retransmit(2..8);

        buf.truncate(9);
        assert_eq!(aggregate_unacked(&buf), b"llo, wo");
        assert_eq!(buf.offset(), 9);
        assert_eq!(get(&buf, 4..9), [&b"o, "[..], &b"wo"[..]]);
        // The lost range is retransmitted, then the unsent byte before the truncation
        assert_eq!(buf.poll_transmit(16), (2..8, true));
        assert_eq!(buf.poll_transmit(16), (8..9, true));
        assert!(!buf.has_unsent_data());

        // Late acks and losses past the truncation are ignored
        buf.ack(9..13);
        buf.retransmit(9..13);
        assert!(!buf.has_unsent_data());
        buf.ack(2..9);
        assert!(buf.is_fully_acked());
        assert_eq!(buf.buffered(), 0);
    }

    #[test]
    fn truncate_acked_prefix() {
        let mut buf = SendBuffer::new();
        buf.write(Bytes::from_static(b"Hello, world!"));
        assert_eq!(buf.poll_transmit(16), (0..8, true));
        buf.ack(0..8);
        buf.truncate(4);
        assert!(buf.is_fully_acked());
        assert!(!buf.has_unsent_data());
        assert_eq!(buf.buffered(), 0);
    }

    fn aggregate_unacked(buf: &SendBuffer) -> Vec<u8> {
        let mut result = Vec::new();
        for segment in buf.unacked_segments.iter() {
            result.extend_from_slice(&segment.data);
        }
        result
    }

    fn get(buf: &SendBuffer, offsets: Range<u64>) -> Vec<&[u8]> {
        buf.get(offsets).collect()
    }
}
