//! RFC 2198 "RTP Payload for Redundant Audio Data" (RED) encode/decode.
//!
//! The RED structure is part of the RTP payload (so it is fully covered by SRTP): N redundant
//! block headers (4 bytes each, F=1: 7-bit PT, 14-bit timestamp offset, 10-bit length), then the
//! primary block header (1 byte, F=0: 7-bit PT), then all block payloads in the same order.

use std::collections::VecDeque;

use crate::rtp_::Pt;

use super::PacketError;

const F_BIT: u8 = 0x80;

/// Largest representable timestamp offset (14-bit field).
const MAX_TS_OFFSET: u32 = 0x3fff;

/// Largest representable redundant block length (10-bit field).
const MAX_BLOCK_LEN: usize = 0x3ff;

/// Maximum redundant blocks accepted in one RED payload. RFC 2198 sets no limit, but real
/// Opus-RED senders use a single level of redundancy (str0m sends one; libwebrtc defaults to one
/// and its receiver caps at 32). A 4-byte redundant header can otherwise pack hundreds of blocks
/// into one datagram, so reject more than this to bound the parse work a malformed or hostile
/// peer can trigger.
const MAX_REDUNDANT_BLOCKS: usize = 32;

/// How many packets back we recover from on receive, and the deepest distance a sender may be
/// configured to send (see `CodecConfig::set_red_distances`). This covers realistic multi-level
/// patterns such as `[1, 3, 5]` while bounding the per-packet recovery work (decode + depayload +
/// seq lookup) a hostile peer can trigger, independent of how many blocks the payload carries.
pub(crate) const MAX_RED_RECOVERY_DEPTH: u64 = 8;

/// A redundant block to prepend to the primary payload (older media).
#[derive(Debug, Clone)]
pub struct RedundantBlock {
    /// Block payload type (7-bit).
    pub pt: u8,
    /// `primary_rtp_timestamp - block_timestamp`, 14-bit (max 16383).
    pub timestamp_offset: u32,
    /// Block payload bytes, 10-bit length (max 1023).
    pub payload: Vec<u8>,
}

impl RedundantBlock {
    /// Whether this block fits RFC 2198's 14-bit offset and 10-bit length fields.
    pub fn fits(&self) -> bool {
        self.timestamp_offset <= MAX_TS_OFFSET && self.payload.len() <= MAX_BLOCK_LEN
    }
}

/// A block parsed out of a RED payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RedBlock<'a> {
    /// Block payload type (7-bit).
    pub pt: u8,
    /// Timestamp offset from the primary (0 for the primary block).
    pub timestamp_offset: u32,
    /// Block payload bytes (no header).
    pub payload: &'a [u8],
    /// Whether this is the primary (final) block.
    pub is_primary: bool,
}

/// Encoder for RFC 2198 RED payloads.
pub struct RedEncoder;

impl RedEncoder {
    /// Encode a RED payload: redundant blocks (oldest first) followed by the primary block.
    ///
    /// Redundant blocks whose offset or length do not fit RFC 2198's 14-bit / 10-bit fields are
    /// skipped (see [`RedundantBlock::fits`]) so the output is always a valid RED payload, never
    /// silently truncated.
    pub fn encode(primary_pt: u8, primary: &[u8], redundant: &[RedundantBlock]) -> Vec<u8> {
        // Cap at the decoder's bound so encoder output always round-trips: `RedDecoder::decode`
        // rejects more than `MAX_REDUNDANT_BLOCKS` redundant blocks as a DoS guard. The internal
        // sender never approaches this (at most `MAX_RED_RECOVERY_DEPTH` levels), so this only
        // clamps public-API misuse.
        let blocks: Vec<&RedundantBlock> = redundant
            .iter()
            .filter(|b| b.fits())
            .take(MAX_REDUNDANT_BLOCKS)
            .collect();

        let mut out = Vec::with_capacity(primary.len() + blocks.len() * 4 + 1);

        for b in &blocks {
            // `fits()` guarantees ts <= 0x3fff and len <= 0x3ff, so no masking is needed.
            let ts = b.timestamp_offset;
            let len = b.payload.len() as u32;
            out.push(F_BIT | (b.pt & 0x7f));
            out.push((ts >> 6) as u8);
            out.push((((ts & 0x3f) << 2) | (len >> 8)) as u8);
            out.push((len & 0xff) as u8);
        }

        // Primary header, F=0.
        out.push(primary_pt & 0x7f);

        for b in &blocks {
            out.extend_from_slice(&b.payload);
        }
        out.extend_from_slice(primary);

        out
    }
}

/// Decoder for RFC 2198 RED payloads.
pub struct RedDecoder;

impl RedDecoder {
    /// Parse a RED payload into its blocks (redundant first, primary last).
    ///
    /// Never panics; returns `Err` on any malformed input. RED payloads come from untrusted
    /// remote peers, so this strictly validates header chaining and block lengths.
    pub fn decode(data: &[u8]) -> Result<Vec<RedBlock<'_>>, PacketError> {
        struct Hdr {
            pt: u8,
            ts_offset: u32,
            len: Option<usize>,
        }

        // Phase 1: parse headers up to and including the primary (F=0) header.
        let mut hdrs: Vec<Hdr> = Vec::new();
        let mut i = 0;
        loop {
            let first = *data.get(i).ok_or(PacketError::ErrRedCorruptedPacket)?;
            if first & F_BIT == 0 {
                hdrs.push(Hdr {
                    pt: first & 0x7f,
                    ts_offset: 0,
                    len: None,
                });
                i += 1;
                break;
            }
            // Bound the redundant blocks (untrusted input) before parsing/allocating more. At this
            // point `hdrs` holds only redundant headers, since the primary (F=0) breaks the loop.
            if hdrs.len() >= MAX_REDUNDANT_BLOCKS {
                return Err(PacketError::ErrRedCorruptedPacket);
            }
            let h = data
                .get(i..i + 4)
                .ok_or(PacketError::ErrRedCorruptedPacket)?;
            let ts_offset = ((h[1] as u32) << 6) | ((h[2] as u32) >> 2);
            let len = (((h[2] as usize) & 0x03) << 8) | (h[3] as usize);
            hdrs.push(Hdr {
                pt: h[0] & 0x7f,
                ts_offset,
                len: Some(len),
            });
            i += 4;
        }

        // Phase 2: slice the payloads in the same order as the headers.
        let mut out = Vec::with_capacity(hdrs.len());
        for h in &hdrs {
            match h.len {
                Some(len) => {
                    let payload = data
                        .get(i..i + len)
                        .ok_or(PacketError::ErrRedCorruptedPacket)?;
                    out.push(RedBlock {
                        pt: h.pt,
                        timestamp_offset: h.ts_offset,
                        payload,
                        is_primary: false,
                    });
                    i += len;
                }
                None => {
                    // Primary block takes the remaining bytes (may be empty).
                    out.push(RedBlock {
                        pt: h.pt,
                        timestamp_offset: 0,
                        payload: &data[i..],
                        is_primary: true,
                    });
                }
            }
        }

        Ok(out)
    }
}

/// The redundant blocks that carry `primary_pt` and sit strictly before the primary.
///
/// RFC 2198 allows a redundant block to use a different PT than the primary; str0m only
/// recovers the negotiated codec, so blocks of another PT are skipped. Which sequence number a
/// block belongs to is decided by the receiving stream (it needs the surrounding received
/// packets, see `StreamRx::red_locate_seq`), not from the offsets alone.
pub(crate) fn red_same_pt_blocks<'a>(
    redundant: &'a [RedBlock<'a>],
    primary_pt: u8,
) -> impl Iterator<Item = &'a RedBlock<'a>> {
    redundant
        .iter()
        .filter(move |b| b.pt == primary_pt && b.timestamp_offset > 0)
}

/// Send-side RFC 2198 RED state. `distances` lists how many packets back each redundant level
/// carries (`[1]` = one level, the previous packet; `[1, 3, 5]` = three levels for higher loss),
/// always sorted, distinct and >= 1. `history` keeps the most recent `max(distances)` payloads.
///
/// This is the send-side companion to [`RedDecoder`]: [`Payloader`][crate::packet::Payloader]
/// stays codec- and RED-agnostic; the `Media` layer owns the `RedSender` and applies it through a
/// [`RedSink`][crate::packet::RedSink].
#[derive(Debug)]
pub(crate) struct RedSender {
    red_pt: Pt,
    primary_pt: Pt,
    distances: Box<[u32]>,
    /// The most recent payloads as `(payload, rtp_time)`, oldest at the front, capped at the
    /// deepest configured distance so it never grows without bound.
    history: VecDeque<(Vec<u8>, u32)>,
}

impl RedSender {
    /// Create a RED sender for `primary_pt`, wrapping into `red_pt`, with the given distance
    /// pattern (must be sorted, distinct and >= 1).
    pub(crate) fn new(red_pt: Pt, primary_pt: Pt, distances: &[u32]) -> Self {
        RedSender {
            red_pt,
            primary_pt,
            distances: distances.into(),
            history: VecDeque::new(),
        }
    }

    /// The RED payload type outgoing packets are sent on.
    pub(crate) fn red_pt(&self) -> Pt {
        self.red_pt
    }

    /// Keep the sender current with the negotiated RED PT, primary PT and distance pattern without
    /// discarding a live talk-spurt's redundancy history. If the pattern changed, adopt it and trim
    /// history to the new deepest distance. `distances` must be sorted, distinct and >= 1.
    pub(crate) fn sync(&mut self, red_pt: Pt, primary_pt: Pt, distances: &[u32]) {
        self.red_pt = red_pt;
        self.primary_pt = primary_pt;
        if *self.distances != *distances {
            self.distances = distances.into();
            self.trim_history();
        }
    }

    /// Wrap the primary `payload` (at `rtp_time`) into a RED payload, prepending one redundant
    /// block per configured distance that has a matching history entry and fits RFC 2198's field
    /// limits (a block over the 10-bit, 1023-byte length field does not fit and is dropped, so RED
    /// adds no protection for audio frames that large). The most recent (shallowest, most valuable)
    /// levels are added first and the deepest are dropped once the encoded packet would exceed
    /// `budget` bytes. That size guard keeps the RED packet within the send MTU, which both avoids
    /// IP fragmentation (a fragmented RED packet loses all its redundancy if any fragment drops) and
    /// prevents overflowing the fixed datagram buffer downstream. Kept blocks are emitted
    /// oldest-first as RFC 2198 requires, then the primary. Updates history.
    pub(crate) fn wrap(&mut self, payload: Vec<u8>, rtp_time: u32, budget: usize) -> Vec<u8> {
        let primary_pt = *self.primary_pt;
        let len = self.history.len();

        // Even the mandatory 1-byte primary RED header must fit the budget. When it cannot (a
        // primary that already fills the MTU), emit the payload unwrapped so RED never pushes the
        // packet past `budget`. The `Media` layer reserves this header byte when packetizing for
        // RED, so in normal operation the primary always leaves room and this only guards a
        // pathological input.
        if payload.len() + 1 > budget {
            self.history.push_back((payload.clone(), rtp_time));
            self.trim_history();
            return payload;
        }

        // Room for redundancy after the primary payload and its 1-byte RED header. The guard above
        // makes the subtraction safe. RFC 2198 headers: 1 byte primary, 4 bytes per redundant block.
        let mut remaining = budget - payload.len() - 1;

        // Add the most recent levels first (distance 1 is the most valuable). Collected here
        // shallowest-first, then reversed to oldest-first for the wire.
        let mut kept: Vec<RedundantBlock> = Vec::new();
        for &d in self.distances.iter() {
            let Some((prev, prev_time)) = len
                .checked_sub(d as usize)
                .and_then(|i| self.history.get(i))
            else {
                continue;
            };
            let block = RedundantBlock {
                pt: primary_pt,
                timestamp_offset: rtp_time.wrapping_sub(*prev_time),
                payload: prev.clone(),
            };
            // A distance whose offset or length overflows RFC 2198's fields is skipped, not sent
            // malformed; the primary still goes out, so this only drops redundancy.
            if !block.fits() {
                continue;
            }
            let cost = 4 + block.payload.len();
            if cost > remaining {
                // Out of budget: stop so the kept (shallowest) levels stay contiguous. The primary
                // and its header are already guaranteed to fit, so the encoded RED stays within
                // `budget`.
                break;
            }
            remaining -= cost;
            kept.push(block);
        }
        // RFC 2198 requires redundant blocks oldest-first (largest distance first), then primary.
        kept.reverse();

        let bytes = RedEncoder::encode(primary_pt, &payload, &kept);
        self.history.push_back((payload, rtp_time));
        self.trim_history();
        bytes
    }

    /// Cap history at the deepest configured distance so it never grows without bound.
    fn trim_history(&mut self) {
        let max_distance = self.distances.last().copied().unwrap_or(1) as usize;
        while self.history.len() > max_distance {
            self.history.pop_front();
        }
    }
}

#[cfg(test)]
mod test {
    use super::*;

    fn red_sender(distances: &[u32]) -> RedSender {
        RedSender::new(Pt::new_with_value(63), Pt::new_with_value(111), distances)
    }

    // A budget large enough not to interfere with the small-payload wrap tests.
    const WRAP_BUDGET: usize = 1200;

    #[test]
    fn red_sender_wrap_builds_redundancy_from_history() {
        let mut red = red_sender(&[1]);

        // First packet: primary-only RED (no history yet).
        let first = red.wrap(vec![1, 2, 3], 1000, WRAP_BUDGET);
        let blocks = RedDecoder::decode(&first).unwrap();
        assert_eq!(blocks.len(), 1);
        assert!(blocks[0].is_primary);
        assert_eq!(blocks[0].payload, &[1, 2, 3]);

        // Second packet: carries the first frame as redundancy.
        let second = red.wrap(vec![4, 5], 1960, WRAP_BUDGET);
        let blocks = RedDecoder::decode(&second).unwrap();
        assert_eq!(blocks.len(), 2);
        assert!(!blocks[0].is_primary);
        assert_eq!(blocks[0].pt, 111);
        assert_eq!(blocks[0].timestamp_offset, 960); // 1960 - 1000
        assert_eq!(blocks[0].payload, &[1, 2, 3]);
        assert!(blocks[1].is_primary);
        assert_eq!(blocks[1].payload, &[4, 5]);
    }

    #[test]
    fn red_sender_wrap_multi_distance_emits_levels_oldest_first() {
        // A [1, 3] pattern: once enough history exists, each packet carries the previous frame
        // (distance 1) and the frame three back (distance 3), emitted oldest-first before primary.
        let mut red = red_sender(&[1, 3]);

        for i in 0..4u32 {
            red.wrap(vec![i as u8], i * 960, WRAP_BUDGET);
        }
        let fifth = red.wrap(vec![42], 4 * 960, WRAP_BUDGET);
        let blocks = RedDecoder::decode(&fifth).unwrap();

        assert_eq!(blocks.len(), 3);
        assert!(!blocks[0].is_primary);
        assert_eq!(blocks[0].timestamp_offset, 3 * 960);
        assert_eq!(blocks[0].payload, &[1]);
        assert!(!blocks[1].is_primary);
        assert_eq!(blocks[1].timestamp_offset, 960);
        assert_eq!(blocks[1].payload, &[3]);
        assert!(blocks[2].is_primary);
        assert_eq!(blocks[2].payload, &[42]);
    }

    #[test]
    fn red_sender_wrap_skips_redundancy_that_overflows_rfc_fields() {
        let mut red = red_sender(&[1]);
        red.wrap(vec![1], 0, WRAP_BUDGET);
        // Offset 16384 overflows the 14-bit field, so no redundant block, just the primary.
        let out = red.wrap(vec![2], 0x4000, WRAP_BUDGET);
        let blocks = RedDecoder::decode(&out).unwrap();
        assert_eq!(blocks.len(), 1);
        assert!(blocks[0].is_primary);
    }

    #[test]
    fn red_sender_wrap_sheds_deepest_redundancy_to_fit_budget() {
        let mut red = red_sender(&[1, 2]);
        let budget = 300;
        let frame = |b: u8| vec![b; 100];

        red.wrap(frame(0xAA), 0, budget); // distance-2 source
        red.wrap(frame(0xBB), 960, budget); // distance-1 source
        let out = red.wrap(frame(0xCC), 1920, budget);

        assert!(
            out.len() <= budget,
            "encoded RED {} > budget {budget}",
            out.len()
        );
        let blocks = RedDecoder::decode(&out).unwrap();
        assert_eq!(blocks.len(), 2, "distance 2 shed to fit budget");
        assert!(!blocks[0].is_primary);
        assert_eq!(
            blocks[0].payload,
            &frame(0xBB)[..],
            "kept the shallowest (distance 1)"
        );
        assert!(blocks[1].is_primary);
        assert_eq!(blocks[1].payload, &frame(0xCC)[..]);

        // Ample budget keeps both levels, confirming the guard is what sheds.
        let mut red = red_sender(&[1, 2]);
        red.wrap(frame(0xAA), 0, WRAP_BUDGET);
        red.wrap(frame(0xBB), 960, WRAP_BUDGET);
        let out = red.wrap(frame(0xCC), 1920, WRAP_BUDGET);
        assert_eq!(
            RedDecoder::decode(&out).unwrap().len(),
            3,
            "all levels fit a large budget"
        );
    }

    #[test]
    fn red_sender_wrap_primary_header_does_not_exceed_budget() {
        let mut red = red_sender(&[1]);
        let out = red.wrap(vec![0xAA; WRAP_BUDGET], 960, WRAP_BUDGET);
        assert!(
            out.len() <= WRAP_BUDGET,
            "RED payload {} > budget {WRAP_BUDGET}",
            out.len()
        );
    }

    #[test]
    fn red_sender_sync_trims_history_on_pattern_change() {
        let mut red = red_sender(&[1, 3]);
        for i in 0..4u32 {
            red.wrap(vec![i as u8], i * 960, WRAP_BUDGET);
        }
        // Shrinking the pattern trims history to the new deepest distance, so the next packet only
        // has a distance-1 block available.
        red.sync(Pt::new_with_value(63), Pt::new_with_value(111), &[1]);
        let out = red.wrap(vec![42], 4 * 960, WRAP_BUDGET);
        let blocks = RedDecoder::decode(&out).unwrap();
        assert_eq!(blocks.len(), 2, "only distance 1 survives the trim");
        assert_eq!(blocks[0].timestamp_offset, 960);
    }

    #[test]
    fn primary_only_roundtrip() {
        let bytes = RedEncoder::encode(96, &[1, 2, 3], &[]);
        assert_eq!(bytes, vec![96, 1, 2, 3]);

        let blocks = RedDecoder::decode(&bytes).unwrap();
        assert_eq!(blocks.len(), 1);
        assert!(blocks[0].is_primary);
        assert_eq!(blocks[0].pt, 96);
        assert_eq!(blocks[0].timestamp_offset, 0);
        assert_eq!(blocks[0].payload, &[1, 2, 3]);
    }

    #[test]
    fn one_redundant_block_roundtrip() {
        let red = [RedundantBlock {
            pt: 96,
            timestamp_offset: 960,
            payload: vec![9, 9],
        }];
        let bytes = RedEncoder::encode(96, &[1, 2, 3], &red);

        // Redundant header for ts_offset=960, len=2: F|pt=0xE0; 960>>6=15, low-6 ts bits and the
        // 2 high len bits are 0, len low byte = 2.
        assert_eq!(&bytes[..4], &[0xE0, 15, 0, 2]);
        assert_eq!(bytes[4], 96); // primary header, F=0
        assert_eq!(&bytes[5..], &[9, 9, 1, 2, 3]); // redundant payload then primary

        let blocks = RedDecoder::decode(&bytes).unwrap();
        assert_eq!(blocks.len(), 2);
        assert!(!blocks[0].is_primary);
        assert_eq!(blocks[0].timestamp_offset, 960);
        assert_eq!(blocks[0].payload, &[9, 9]);
        assert!(blocks[1].is_primary);
        assert_eq!(blocks[1].payload, &[1, 2, 3]);
    }

    #[test]
    fn encode_skips_oversized_blocks() {
        // A block whose offset (>14 bits) or length (>10 bits) does not fit is dropped, so the
        // wire payload is never silently corrupted — even in release builds.
        let bad_offset = RedundantBlock {
            pt: 96,
            timestamp_offset: 0x4000, // > 0x3fff
            payload: vec![1, 2],
        };
        let bad_len = RedundantBlock {
            pt: 96,
            timestamp_offset: 100,
            payload: vec![0u8; 2000], // > 0x3ff
        };
        let bytes = RedEncoder::encode(96, &[7, 7], &[bad_offset, bad_len]);

        let blocks = RedDecoder::decode(&bytes).unwrap();
        assert_eq!(blocks.len(), 1, "oversized blocks must be skipped");
        assert!(blocks[0].is_primary);
        assert_eq!(blocks[0].payload, &[7, 7]);
    }

    #[test]
    fn truncated_header_is_error_not_panic() {
        assert!(RedDecoder::decode(&[0x80, 0x00]).is_err()); // F=1 but < 4 header bytes
        assert!(RedDecoder::decode(&[]).is_err()); // empty
    }

    #[test]
    fn block_length_past_end_is_error() {
        // redundant header claims len=200 (len hi byte 0, lo byte 200) but no payload follows
        let b = [0x80 | 96, 0, 0, 200, 96];
        assert!(RedDecoder::decode(&b).is_err());
    }

    #[test]
    fn never_panics_on_arbitrary_input() {
        // Exhaustively poke short inputs; the decoder must only ever return Ok/Err.
        for a in 0u16..=255 {
            for b in 0u16..=255 {
                let _ = RedDecoder::decode(&[a as u8, b as u8]);
            }
        }
    }

    #[test]
    fn decode_rejects_too_many_redundant_blocks() {
        // One 4-byte redundant header per block lets a single datagram pack hundreds of blocks.
        // The decoder must bound this so a hostile peer can't expand one packet into an unbounded
        // number of recovered packets. `n` zero-length redundant blocks, then an empty primary.
        fn red_with_redundant_blocks(n: usize) -> Vec<u8> {
            let mut data = Vec::with_capacity(n * 4 + 1);
            for _ in 0..n {
                data.extend_from_slice(&[F_BIT | 96, 0, 0, 0]); // F=1, pt=96, ts=0, len=0
            }
            data.push(96); // primary header, F=0
            data
        }

        // At the limit: MAX redundant + 1 primary still decodes.
        let ok = red_with_redundant_blocks(MAX_REDUNDANT_BLOCKS);
        let blocks = RedDecoder::decode(&ok).expect("max redundant blocks is allowed");
        assert_eq!(blocks.len(), MAX_REDUNDANT_BLOCKS + 1);

        // One over the limit, and a pathological count, are rejected (not decoded).
        assert!(RedDecoder::decode(&red_with_redundant_blocks(MAX_REDUNDANT_BLOCKS + 1)).is_err());
        assert!(RedDecoder::decode(&red_with_redundant_blocks(360)).is_err());
    }

    #[test]
    fn same_pt_blocks_skip_foreign_pt_and_primary_offset() {
        let block = |pt, ts| RedBlock {
            pt,
            timestamp_offset: ts,
            payload: &[0u8],
            is_primary: false,
        };
        let redundant = vec![
            block(111, 1920),
            block(96, 960),
            block(111, 0),
            block(111, 960),
        ];
        let offsets: Vec<u32> = red_same_pt_blocks(&redundant, 111)
            .map(|b| b.timestamp_offset)
            .collect();
        assert_eq!(offsets, vec![1920, 960]);
    }

    #[test]
    fn public_encoder_output_is_accepted_by_public_decoder() {
        let redundant: Vec<_> = (0..33)
            .map(|i| RedundantBlock {
                pt: 111,
                timestamp_offset: i + 1,
                payload: vec![i as u8],
            })
            .collect();

        let encoded = RedEncoder::encode(111, &[0xAA], &redundant);
        assert!(
            RedDecoder::decode(&encoded).is_ok(),
            "the public decoder must accept packets produced by the public encoder"
        );
    }
}
