//! H266 (VVC) RTP packetization per RFC 9328.
//!
//! Wire formats implemented (RFC 9328 §4.3):
//! - Single NAL unit packets (§4.3.1).
//! - Aggregation Packets, NAL type 28 (§4.3.2).
//! - Fragmentation Units, NAL type 29 (§4.3.3), FU header
//!   `S(1) | E(1) | P(1) | FuType(5)`.
//!
//! The two-byte NAL unit header is `F(1) | Z(1) | LayerId(6) || Type(5) |
//! TID(3)` (§1.1.4) — the NAL type lives in the SECOND byte, upper 5 bits.
//! Parameter sets: VPS=14, SPS=15, PPS=16. Skipped on send: AUD=20.
//! IRAP (keyframe) NAL types: IDR_W_RADL=7, IDR_N_LP=8, CRA=9.
//!
//! Profile/tier/level fmtp negotiation (RFC 9328 §7.2) is implemented in
//! `h266_profile.rs` and the format layer. DONL (`sprop-max-don-diff`,
//! §7.2) is supported for send and receive via `with_donl`, wired from SDP
//! negotiation.

use super::{CodecExtra, Depacketizer, PacketError, Packetizer};
use tracing::warn;

pub static ANNEXB_NALUSTART_CODE: &[u8] = &[0x00, 0x00, 0x00, 0x01];

/// H266 NAL unit header size (2 bytes).
const H266NALU_HEADER_SIZE: usize = 2;
/// FU header size (1 byte).
const H266FRAGMENTATION_UNIT_HEADER_SIZE: usize = 1;

// H266 NAL unit types (ITU-T H.266 Table 5).
const H266NALU_IDR_W_RADL: u8 = 7;
const H266NALU_IDR_N_LP: u8 = 8;
const H266NALU_CRA: u8 = 9;
#[allow(dead_code)] // RFC 9328 NAL type table completeness; referenced by tests.
const H266NALU_GDR: u8 = 10;
const H266NALU_VPS_NALU_TYPE: u8 = 14;
const H266NALU_SPS_NALU_TYPE: u8 = 15;
const H266NALU_PPS_NALU_TYPE: u8 = 16;
const H266NALU_AUD_NALU_TYPE: u8 = 20;
#[allow(dead_code)] // FD_NUT; referenced by tests.
const H266NALU_FILLER_NALU_TYPE: u8 = 25;

// RFC 9328 RTP packet types.
const H266NALU_AGGREGATION_PACKET_TYPE: u8 = 28;
const H266NALU_FRAGMENTATION_UNIT_TYPE: u8 = 29;

const MAX_PACKET_SIZE: usize = 1200;
const MIN_FU_PAYLOAD: usize = 1;
const MIN_MTU: usize = H266NALU_HEADER_SIZE + H266FRAGMENTATION_UNIT_HEADER_SIZE + MIN_FU_PAYLOAD;

/// Codec extra reported alongside depacketized H266 data.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct H266CodecExtra {
    /// Whether the depacketized data contains an IRAP NAL (keyframe).
    pub is_keyframe: bool,
}

/// H266 NAL unit header (RFC 9328 §1.1.4).
///
/// ```text
/// +---------------+---------------+
/// |0|1|2|3|4|5|6|7|0|1|2|3|4|5|6|7|
/// +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
/// |F|Z|  LayerID  |  Type   | TID |
/// +---------------+---------------+
/// ```
#[derive(Default, Debug, Copy, Clone, PartialEq, Eq)]
pub struct H266NALUHeader(pub u16);

impl H266NALUHeader {
    pub fn new(high_byte: u8, low_byte: u8) -> Self {
        H266NALUHeader(((high_byte as u16) << 8) | low_byte as u16)
    }

    /// Construct a header from NAL type, layer id and temporal id.
    pub fn new_with_type(typ: u8, layer_id: u8, tid: u8) -> Self {
        let b0 = layer_id & 0b0011_1111;
        let b1 = ((typ & 0b1_1111) << 3) | (tid & 0b111);
        Self::new(b0, b1)
    }

    /// Forbidden zero bit.
    pub fn f(&self) -> bool {
        (self.0 >> 15) & 0b1 != 0
    }

    /// nuh_reserved_zero_bit (Z).
    pub fn z(&self) -> bool {
        (self.0 >> 14) & 0b1 != 0
    }

    /// nuh_layer_id (6 bits of the first byte).
    pub fn layer_id(&self) -> u8 {
        ((self.0 >> 8) & 0b0011_1111) as u8
    }

    /// NAL unit type — upper 5 bits of the SECOND byte.
    pub fn nalu_type(&self) -> u8 {
        ((self.0 >> 3) & 0b1_1111) as u8
    }

    /// nuh_temporal_id_plus1 — lower 3 bits of the second byte.
    pub fn tid(&self) -> u8 {
        (self.0 & 0b111) as u8
    }

    /// IRAP picture (keyframe): IDR_W_RADL, IDR_N_LP, CRA.
    /// Note: GDR (gradual decoding refresh) is a recovery point but not an
    /// instantaneous keyframe, so it is deliberately excluded.
    pub fn is_irap(&self) -> bool {
        matches!(
            self.nalu_type(),
            H266NALU_IDR_W_RADL | H266NALU_IDR_N_LP | H266NALU_CRA
        )
    }

    pub fn is_aggregation_packet(&self) -> bool {
        self.nalu_type() == H266NALU_AGGREGATION_PACKET_TYPE
    }

    pub fn is_fragmentation_unit(&self) -> bool {
        self.nalu_type() == H266NALU_FRAGMENTATION_UNIT_TYPE
    }
}

/// H266 FU header: `S(1) | E(1) | P(1) | FuType(5)` (RFC 9328 §4.3.3).
#[derive(Default, Debug, Copy, Clone, PartialEq, Eq)]
pub struct H266FragmentationUnitHeader(pub u8);

impl H266FragmentationUnitHeader {
    pub fn new(start: bool, end: bool, last_of_picture: bool, fu_type: u8) -> Self {
        let mut b = fu_type & 0b1_1111;
        if start {
            b |= 0b1000_0000;
        }
        if end {
            b |= 0b0100_0000;
        }
        if last_of_picture {
            b |= 0b0010_0000;
        }
        H266FragmentationUnitHeader(b)
    }

    /// Start of fragmented NAL unit.
    pub fn s(&self) -> bool {
        self.0 & 0b1000_0000 != 0
    }

    /// End of fragmented NAL unit.
    pub fn e(&self) -> bool {
        self.0 & 0b0100_0000 != 0
    }

    /// Last FU of the last VCL NAL of the picture.
    #[allow(dead_code)] // RFC 9328 FU header accessor; exercised by tests.
    pub fn p(&self) -> bool {
        self.0 & 0b0010_0000 != 0
    }

    /// Original NAL unit type.
    pub fn fu_type(&self) -> u8 {
        self.0 & 0b1_1111
    }
}

/// Detect whether an RTP payload contains an H266 keyframe (IRAP).
///
/// Note: assumes payloads without DONL fields. When `sprop-max-don-diff > 0`
/// is negotiated, Aggregation Packet payloads carry a 16-bit DONL before the
/// first unit, which this scan does not account for (single NAL and FU
/// detection are unaffected — the inspected bytes precede the DONL).
pub fn detect_h266_keyframe(payload: &[u8]) -> bool {
    if payload.len() < H266NALU_HEADER_SIZE {
        return false;
    }

    let header = H266NALUHeader::new(payload[0], payload[1]);
    match header.nalu_type() {
        H266NALU_AGGREGATION_PACKET_TYPE => {
            // Check all aggregated NAL units.
            let mut offset = H266NALU_HEADER_SIZE;
            while offset + 2 <= payload.len() {
                let nalu_size = ((payload[offset] as usize) << 8) | payload[offset + 1] as usize;
                offset += 2;
                if offset + nalu_size > payload.len() || nalu_size < H266NALU_HEADER_SIZE {
                    break;
                }
                let inner = H266NALUHeader::new(payload[offset], payload[offset + 1]);
                if inner.is_irap() {
                    return true;
                }
                offset += nalu_size;
            }
            false
        }
        H266NALU_FRAGMENTATION_UNIT_TYPE => {
            if payload.len() < H266NALU_HEADER_SIZE + 1 {
                return false;
            }
            let fu = H266FragmentationUnitHeader(payload[2]);
            let t = fu.fu_type();
            fu.s() && (H266NALU_IDR_W_RADL..=H266NALU_CRA).contains(&t)
        }
        _ => header.is_irap(),
    }
}

/// H266 packetizer (RFC 9328): Single NAL / AP (28) / FU (29).
#[derive(Debug)]
pub struct H266Packetizer {
    /// Cached parameter sets, sent as an AP before the next non-param NAL.
    vps_nalu: Option<Vec<u8>>,
    sps_nalu: Option<Vec<u8>>,
    pps_nalu: Option<Vec<u8>>,
    /// DONL counter (RFC 9328 §4.3): present when sprop-max-don-diff > 0.
    donl: Option<u16>,
    /// Reusable packet build buffer.
    pkt_buf: Vec<u8>,
}

impl Default for H266Packetizer {
    fn default() -> Self {
        H266Packetizer {
            vps_nalu: None,
            sps_nalu: None,
            pps_nalu: None,
            donl: None,
            pkt_buf: Vec::with_capacity(MAX_PACKET_SIZE),
        }
    }
}

impl H266Packetizer {
    /// Enable/disable DONL field emission (RFC 9328 §4.3, sprop-max-don-diff > 0).
    pub fn with_donl(&mut self, value: bool) {
        self.donl = if value { Some(0) } else { None };
    }

    fn increment_donl(&mut self) {
        if let Some(ref mut donl) = self.donl {
            *donl = donl.wrapping_add(1);
        }
    }

    fn increment_donl_by(&mut self, n: u16) {
        if let Some(ref mut donl) = self.donl {
            *donl = donl.wrapping_add(n);
        }
    }

    /// Build an AP (type 28) from NAL units into `pkt_buf`.
    /// Returns false if the AP would exceed the MTU (or a unit cannot be
    /// represented with a 16-bit length).
    fn build_ap_packet(
        nal_units: &[&[u8]],
        donl: Option<u16>,
        buf: &mut Vec<u8>,
        mtu: usize,
    ) -> bool {
        // AP PayloadHdr (RFC 9328 §4.3.2): F MUST be 0 if the F bit of each
        // aggregated NAL unit is 0 (OR otherwise); Z is reserved (0);
        // LayerId and TID MUST be the lowest LayerId/TID of all the
        // aggregated NAL units.
        let mut f_bit = false;
        let mut min_layer = 0b0011_1111u8;
        let mut min_tid = 0b111u8;
        for n in nal_units {
            let h = H266NALUHeader::new(n[0], n[1]);
            f_bit |= h.f();
            min_layer = min_layer.min(h.layer_id());
            min_tid = min_tid.min(h.tid());
        }
        // TID is nuh_temporal_id_plus1: never emit 0.
        let tid = min_tid.max(1);

        // Aggregation unit lengths are 16-bit (RFC 9328 §4.3.2).
        if nal_units.iter().any(|n| n.len() > u16::MAX as usize) {
            return false;
        }

        // DONL (2 bytes) precedes the first aggregation unit when enabled
        // (RFC 9328 §4.3.2).
        let donl_overhead = if donl.is_some() { 2 } else { 0 };
        let total: usize = H266NALU_HEADER_SIZE
            + donl_overhead
            + nal_units.iter().map(|n| 2 + n.len()).sum::<usize>();
        if total > mtu {
            return false;
        }

        buf.clear();
        let ap_hdr =
            H266NALUHeader::new_with_type(H266NALU_AGGREGATION_PACKET_TYPE, min_layer, tid);
        let [b0, b1] = ap_hdr.0.to_be_bytes();
        buf.push(b0 | ((f_bit as u8) << 7)); // F | Z=0 | LayerId
        buf.push(b1);

        // Write DONL for the first aggregation unit if present (RFC 9328 §4.3.2).
        if let Some(donl_value) = donl {
            buf.extend_from_slice(&donl_value.to_be_bytes());
        }

        for n in nal_units {
            buf.extend_from_slice(&(n.len() as u16).to_be_bytes());
            buf.extend_from_slice(n);
        }
        true
    }

    /// Find the next Annex-B start code at/after `start`.
    /// Returns (offset, length) or (-1, -1).
    fn next_start_code(payload: &[u8], start: usize) -> (isize, isize) {
        let mut i = start;
        while i + 3 <= payload.len() {
            if payload[i] == 0 && payload[i + 1] == 0 {
                if payload[i + 2] == 1 {
                    return (i as isize, 3);
                }
                if i + 4 <= payload.len() && payload[i + 2] == 0 && payload[i + 3] == 1 {
                    return (i as isize, 4);
                }
            }
            i += 1;
        }
        (-1, -1)
    }

    /// Emit one NAL unit as RTP payload(s):
    /// param sets -> cached -> AP(28); small -> Single NAL; large -> FU(29).
    fn emit_nalu(&mut self, nalu: &[u8], mtu: usize, out: &mut Vec<Vec<u8>>) {
        if mtu == 0 || nalu.len() < H266NALU_HEADER_SIZE {
            return;
        }

        let original_hdr = H266NALUHeader::new(nalu[0], nalu[1]);
        let original_type = original_hdr.nalu_type();

        // Ignore AUD.
        if original_type == H266NALU_AUD_NALU_TYPE {
            return;
        }

        // NAL types 28 (AP) and 29 (FU) are reserved for RTP packetization
        // (RFC 9328 §4.3) and must never appear in an input bitstream.
        if original_type == H266NALU_AGGREGATION_PACKET_TYPE
            || original_type == H266NALU_FRAGMENTATION_UNIT_TYPE
        {
            warn!(
                "H266-PKT skipping reserved NAL type {} in input bitstream",
                original_type
            );
            return;
        }

        // Cache parameter sets; send them as AP before the next other NALU.
        match original_type {
            H266NALU_VPS_NALU_TYPE => {
                self.vps_nalu = Some(nalu.to_vec());
                return;
            }
            H266NALU_SPS_NALU_TYPE => {
                self.sps_nalu = Some(nalu.to_vec());
                return;
            }
            H266NALU_PPS_NALU_TYPE => {
                self.pps_nalu = Some(nalu.to_vec());
                return;
            }
            _ => {}
        }

        // Emit cached parameter sets as an AP (or singles fallback).
        // Note: VPS is optional in VVC streams — SPS+PPS is enough to emit.
        if self.sps_nalu.is_some() && self.pps_nalu.is_some() {
            let mut nal_units_arr: [&[u8]; 3] = [&[], &[], &[]];
            let mut count = 0;
            if let Some(vps) = &self.vps_nalu {
                nal_units_arr[count] = vps;
                count += 1;
            }
            if let Some(sps) = &self.sps_nalu {
                nal_units_arr[count] = sps;
                count += 1;
            }
            if let Some(pps) = &self.pps_nalu {
                nal_units_arr[count] = pps;
                count += 1;
            }
            let nal_units = &nal_units_arr[..count];

            if count >= 2 && Self::build_ap_packet(nal_units, self.donl, &mut self.pkt_buf, mtu) {
                out.push(self.pkt_buf.clone());
                // One DONL per aggregated NAL unit.
                self.increment_donl_by(count as u16);
            } else {
                // Fall back to individual Single NAL packets.
                let fallback_donl_overhead = if self.donl.is_some() { 2 } else { 0 };
                for nal_unit in nal_units {
                    if nal_unit.len() + fallback_donl_overhead <= mtu {
                        if let Some(ref mut donl_value) = self.donl {
                            self.pkt_buf.clear();
                            self.pkt_buf
                                .extend_from_slice(&nal_unit[..H266NALU_HEADER_SIZE]);
                            self.pkt_buf.extend_from_slice(&donl_value.to_be_bytes());
                            self.pkt_buf
                                .extend_from_slice(&nal_unit[H266NALU_HEADER_SIZE..]);
                            out.push(self.pkt_buf.clone());
                            *donl_value = donl_value.wrapping_add(1);
                        } else {
                            out.push(nal_unit.to_vec());
                        }
                    }
                }
            }

            self.vps_nalu = None;
            self.sps_nalu = None;
            self.pps_nalu = None;
        }

        // Single NAL unit packet (RFC 9328 §4.3.1).
        // With DONL enabled, 2 extra bytes follow the NAL header.
        let donl_overhead = if self.donl.is_some() { 2 } else { 0 };
        if nalu.len() + donl_overhead <= mtu {
            if let Some(donl_value) = self.donl {
                self.pkt_buf.clear();
                self.pkt_buf
                    .extend_from_slice(&nalu[..H266NALU_HEADER_SIZE]);
                self.pkt_buf.extend_from_slice(&donl_value.to_be_bytes());
                self.pkt_buf
                    .extend_from_slice(&nalu[H266NALU_HEADER_SIZE..]);
                out.push(self.pkt_buf.clone());
                self.increment_donl();
            } else {
                out.push(nalu.to_vec());
            }
            return;
        }

        // Fragmentation Units (RFC 9328 §4.3.3).
        const FU_OVERHEAD: usize = H266NALU_HEADER_SIZE + H266FRAGMENTATION_UNIT_HEADER_SIZE;
        if nalu.len() <= H266NALU_HEADER_SIZE {
            return;
        }

        // FU payload header: copy of the original header with Type replaced
        // by 29. Type lives in the SECOND byte, bits 7..3.
        let fu_b0 = nalu[0];
        let fu_b1 = (H266NALU_FRAGMENTATION_UNIT_TYPE << 3) | (nalu[1] & 0b111);

        let payload = &nalu[H266NALU_HEADER_SIZE..];
        let effective_mtu = mtu.min(MAX_PACKET_SIZE);
        // The first fragment also carries the DONL field when enabled
        // (RFC 9328 §4.3.3).
        if effective_mtu <= FU_OVERHEAD + donl_overhead {
            return;
        }
        let first_max = effective_mtu - FU_OVERHEAD - donl_overhead;
        let max_fragment = effective_mtu - FU_OVERHEAD;
        let donl_bytes = self.donl.map(u16::to_be_bytes);

        let mut offset = 0;
        while offset < payload.len() {
            let first = offset == 0;
            let remaining = payload.len() - offset;
            let budget = if first { first_max } else { max_fragment };
            let take = remaining.min(budget);
            let end = offset + take == payload.len();

            // P bit (last FU of the last VCL NAL of a picture): RFC 9328
            // §4.3.3 only mandates P=0 when the FU is NOT that last
            // fragment; always sending 0 is compliant. Not tracked here.
            let fu_hdr = H266FragmentationUnitHeader::new(first, end, false, original_type);

            self.pkt_buf.clear();
            self.pkt_buf.push(fu_b0);
            self.pkt_buf.push(fu_b1);
            self.pkt_buf.push(fu_hdr.0);
            if first {
                if let Some(ref b) = donl_bytes {
                    self.pkt_buf.extend_from_slice(b);
                }
            }
            self.pkt_buf
                .extend_from_slice(&payload[offset..offset + take]);
            out.push(self.pkt_buf.clone());
            offset += take;
        }

        // One DONL per NAL unit (not per fragment).
        self.increment_donl();
    }
}

impl Packetizer for H266Packetizer {
    fn packetize(&mut self, mtu: usize, payload: &[u8]) -> Result<Vec<Vec<u8>>, PacketError> {
        if payload.is_empty() {
            return Ok(vec![]);
        }

        let mtu = match mtu {
            0 => {
                warn!("MTU is 0, cannot packetize H.266");
                return Ok(vec![]);
            }
            mtu if mtu > MAX_PACKET_SIZE => {
                warn!(
                    "MTU {} exceeds MAX_PACKET_SIZE {}, clamping",
                    mtu, MAX_PACKET_SIZE
                );
                MAX_PACKET_SIZE
            }
            mtu if mtu < MIN_MTU => {
                warn!("MTU {} too small for H.266 fragmentation", mtu);
                return Ok(vec![]);
            }
            mtu => mtu,
        };

        // Pre-allocate with estimated capacity to avoid reallocations
        // in the hot path (one entry per expected RTP packet).
        let estimated_packets = payload
            .len()
            .checked_div(mtu.saturating_sub(3))
            .unwrap_or(1)
            .saturating_add(4);
        let mut packets = Vec::with_capacity(estimated_packets);

        // No start codes: treat the whole payload as one NAL unit.
        let (mut next_start, mut next_len) = Self::next_start_code(payload, 0);
        if next_start == -1 {
            self.emit_nalu(payload, mtu, &mut packets);
            return Ok(packets);
        }

        // Walk the Annex-B bytestream.
        while next_start != -1 {
            let nalu_start = (next_start + next_len) as usize;
            let (s2, l2) = Self::next_start_code(payload, nalu_start);
            next_start = s2;
            next_len = l2;

            if next_start != -1 {
                self.emit_nalu(&payload[nalu_start..next_start as usize], mtu, &mut packets);
            } else {
                self.emit_nalu(&payload[nalu_start..], mtu, &mut packets);
            }
        }

        Ok(packets)
    }

    fn is_marker(&mut self, _data: &[u8], _previous: Option<&[u8]>, last: bool) -> bool {
        last
    }
}

/// H266 depacketizer (RFC 9328): Single NAL / AP (28) / FU (29).
/// Emits Annex-B (start-code prefixed) NAL units.
#[derive(Debug, Default)]
pub struct H266Depacketizer {
    /// Reassembly buffer for fragmentation units.
    fu_buffer: Option<Vec<u8>>,
    /// Whether DONL fields are present (sprop-max-don-diff > 0).
    might_need_donl: bool,
}

impl H266Depacketizer {
    /// Enable/disable DONL field parsing (RFC 9328 §4.3, sprop-max-don-diff > 0).
    pub fn with_donl(&mut self, value: bool) {
        self.might_need_donl = value;
    }
}

impl Depacketizer for H266Depacketizer {
    fn out_size_hint(&self, packets_size: usize) -> Option<usize> {
        let estimated_packets = (packets_size / 1200).saturating_add(1);
        Some(packets_size.saturating_add(4usize.saturating_mul(estimated_packets)))
    }

    fn depacketize(
        &mut self,
        packet: &[u8],
        out: &mut Vec<u8>,
        codec_extra: &mut CodecExtra,
    ) -> Result<(), PacketError> {
        if packet.len() <= H266NALU_HEADER_SIZE {
            return Err(PacketError::ErrShortPacket);
        }

        let header = H266NALUHeader::new(packet[0], packet[1]);
        if header.f() {
            return Err(PacketError::ErrH266CorruptedPacket);
        }
        if header.z() {
            // nuh_reserved_zero_bit: required to be 0 (RFC 9328 §1.1.4,
            // reserved for future extensions). Tolerated and ignored here.
            warn!("H266-DEPKT NAL with reserved Z bit set (ignored)");
        }

        let mark_keyframe = |codec_extra: &mut CodecExtra, irap: bool| {
            let is_keyframe = if let CodecExtra::H266(e) = codec_extra {
                irap | e.is_keyframe
            } else {
                irap
            };
            *codec_extra = CodecExtra::H266(H266CodecExtra { is_keyframe });
        };

        if header.is_fragmentation_unit() {
            if packet.len() < H266NALU_HEADER_SIZE + 1 {
                return Err(PacketError::ErrShortPacket);
            }
            let fu_header = H266FragmentationUnitHeader(packet[2]);

            // The first FU of a series carries the DONL field when enabled
            // (RFC 9328 §4.3.3) — skip it.
            let payload_start = if fu_header.s() && self.might_need_donl {
                if packet.len() < H266NALU_HEADER_SIZE + 1 + 2 {
                    return Err(PacketError::ErrShortPacket);
                }
                H266NALU_HEADER_SIZE + 1 + 2
            } else {
                H266NALU_HEADER_SIZE + 1
            };
            let fu_payload = &packet[payload_start..];

            if fu_header.s() {
                match &mut self.fu_buffer {
                    Some(buf) => buf.clear(),
                    None => self.fu_buffer = Some(Vec::with_capacity(128 * 1024)),
                }
            }

            if let Some(ref mut buf) = self.fu_buffer {
                buf.extend_from_slice(fu_payload);
            }

            if fu_header.e() {
                if let Some(ref buf) = self.fu_buffer {
                    // Rebuild the original NAL header: byte0 unchanged,
                    // byte1 = orig type in bits 7..3 + original TID.
                    let orig_type = fu_header.fu_type();
                    let orig_b0 = packet[0];
                    let orig_b1 = (orig_type << 3) | (packet[1] & 0b111);
                    let irap = matches!(
                        orig_type,
                        H266NALU_IDR_W_RADL | H266NALU_IDR_N_LP | H266NALU_CRA
                    );
                    mark_keyframe(codec_extra, irap);

                    out.extend_from_slice(ANNEXB_NALUSTART_CODE);
                    out.push(orig_b0);
                    out.push(orig_b1);
                    out.extend_from_slice(buf);
                }
            }
            Ok(())
        } else if header.is_aggregation_packet() {
            let mut offset = H266NALU_HEADER_SIZE;
            let mut unit_count = 0;

            // DONL precedes the first aggregation unit when enabled
            // (RFC 9328 §4.3.2).
            if self.might_need_donl {
                if packet.len() < offset + 2 {
                    return Err(PacketError::ErrShortPacket);
                }
                offset += 2;
            }

            // Remember where this AP's output started so a malformed
            // packet doesn't leave partial NAL units in `out`.
            let out_start = out.len();

            while offset < packet.len() {
                if offset + 2 > packet.len() {
                    out.truncate(out_start);
                    return Err(PacketError::ErrShortPacket);
                }
                let nalu_size = ((packet[offset] as usize) << 8) | (packet[offset + 1] as usize);
                offset += 2;
                if offset + nalu_size > packet.len() || nalu_size < H266NALU_HEADER_SIZE {
                    // Length field inconsistent with the remaining bytes.
                    out.truncate(out_start);
                    return Err(PacketError::ErrShortPacket);
                }
                let nalu = &packet[offset..offset + nalu_size];
                offset += nalu_size;
                unit_count += 1;

                let inner = H266NALUHeader::new(nalu[0], nalu[1]);

                // NAL types 28 (AP) and 29 (FU) are RTP-only constructs and
                // must never appear inside an aggregation packet
                // (RFC 9328 §4.3.2).
                if inner.is_aggregation_packet() || inner.is_fragmentation_unit() {
                    out.truncate(out_start);
                    return Err(PacketError::ErrH266CorruptedPacket);
                }

                mark_keyframe(codec_extra, inner.is_irap());

                out.extend_from_slice(ANNEXB_NALUSTART_CODE);
                out.extend_from_slice(nalu);
            }

            if unit_count < 2 {
                return Err(PacketError::ErrShortPacket);
            }

            Ok(())
        } else {
            // Single NAL unit packet.
            mark_keyframe(codec_extra, header.is_irap());

            out.extend_from_slice(ANNEXB_NALUSTART_CODE);
            if self.might_need_donl {
                // DONL (2 bytes) sits between the NAL header and the payload
                // (RFC 9328 §4.3.1) — strip it.
                if packet.len() < H266NALU_HEADER_SIZE + 2 + 1 {
                    return Err(PacketError::ErrShortPacket);
                }
                out.extend_from_slice(&packet[..H266NALU_HEADER_SIZE]);
                out.extend_from_slice(&packet[H266NALU_HEADER_SIZE + 2..]);
            } else {
                out.extend_from_slice(packet);
            }
            Ok(())
        }
    }

    fn is_partition_head(&self, payload: &[u8]) -> bool {
        if payload.len() < H266NALU_HEADER_SIZE {
            return false;
        }
        let header = H266NALUHeader::new(payload[0], payload[1]);
        if header.f() {
            return true;
        }
        // Single NAL and AP packets are partition heads.
        if !header.is_fragmentation_unit() {
            return true;
        }
        // FU: head only when the S bit is set.
        if payload.len() < H266NALU_HEADER_SIZE + 1 {
            return false;
        }
        H266FragmentationUnitHeader(payload[2]).s()
    }

    fn is_partition_tail(&self, marker: bool, payload: &[u8]) -> bool {
        if payload.len() < H266NALU_HEADER_SIZE {
            return false;
        }
        let header = H266NALUHeader::new(payload[0], payload[1]);
        if header.is_fragmentation_unit() {
            if payload.len() < H266NALU_HEADER_SIZE + 1 {
                return false;
            }
            return H266FragmentationUnitHeader(payload[2]).e();
        }
        marker
    }
}

#[cfg(test)]
mod test {
    use super::*;

    type Result<T> = std::result::Result<T, PacketError>;

    // ========== Shared Test Utilities ==========

    /// Build a 2-byte H.266 NAL header: type in byte 1 bits 7..3.
    fn hdr(typ: u8, layer: u8, tid: u8) -> [u8; 2] {
        let h = H266NALUHeader::new_with_type(typ, layer, tid);
        h.0.to_be_bytes()
    }

    /// Annex-B encode a list of NAL units with 4-byte start codes.
    fn annexb(nals: &[&[u8]]) -> Vec<u8> {
        let mut out = Vec::new();
        for n in nals {
            out.extend_from_slice(&[0, 0, 0, 1]);
            out.extend_from_slice(n);
        }
        out
    }

    /// Make a NAL unit of `len` total bytes with the given type.
    fn make_nal(typ: u8, len: usize) -> Vec<u8> {
        assert!(len >= 2);
        let mut n = hdr(typ, 0, 1).to_vec();
        for i in 0..len - 2 {
            n.push((i % 251) as u8);
        }
        n
    }

    /// Make a NAL unit of `len` total bytes with type, layer id and tid.
    fn make_nal_lt(typ: u8, len: usize, layer: u8, tid: u8) -> Vec<u8> {
        assert!(len >= 2);
        let mut n = hdr(typ, layer, tid).to_vec();
        for i in 0..len - 2 {
            n.push((i % 251) as u8);
        }
        n
    }

    /// Reconstruct the original NAL unit from a sequence of FU packets.
    /// Assumes all packets are FU (type 29) and are consecutive.
    fn reconstruct_from_fu_packets(packets: &[Vec<u8>]) -> Vec<u8> {
        let mut out = Vec::new();
        let mut started = false;

        for pkt in packets {
            assert!(pkt.len() >= 3);
            let h = H266NALUHeader::new(pkt[0], pkt[1]);
            assert!(h.is_fragmentation_unit());

            let fu = H266FragmentationUnitHeader(pkt[2]);

            if fu.s() {
                // Rebuild the original 2-byte NAL header: byte 0 unchanged,
                // byte 1 = fu_type in bits 7..3 + original TID.
                out.push(pkt[0]);
                out.push((fu.fu_type() << 3) | (pkt[1] & 0b111));
                started = true;
            }

            assert!(started, "FU sequence must start with S=1");
            out.extend_from_slice(&pkt[3..]);
        }

        out
    }

    /// Depacketize a list of RTP payloads, returning the Annex-B output
    /// and whether any keyframe was flagged.
    fn depacketize_all(packets: &[Vec<u8>]) -> Result<(Vec<u8>, bool)> {
        let mut depack = H266Depacketizer::default();
        let mut out = Vec::new();
        let mut keyframe = false;
        for p in packets {
            let mut extra = CodecExtra::None;
            depack.depacketize(p, &mut out, &mut extra)?;
            if let CodecExtra::H266(e) = extra {
                keyframe |= e.is_keyframe;
            }
        }
        Ok((out, keyframe))
    }

    /// RFC 9328 bitfield correctness tests.
    mod header_tests {
        use super::*;

        /// Test H.266 NAL Unit header parsing and field extraction.
        /// Verifies F bit, NAL type, layer_id, tid, and packet type
        /// detection (AP/FU).
        #[test]
        fn test_h266_nalu_header() -> Result<()> {
            #[derive(Default)]
            struct TestType {
                raw_header: &'static [u8],

                fbit: bool,
                typ: u8,
                layer_id: u8,
                tid: u8,

                is_ap: bool,
                is_fu: bool,
            }

            let tests = vec![
                // fbit (bit 7 of byte 0)
                TestType {
                    raw_header: &[0x80, 0x00],
                    typ: 0,
                    layer_id: 0,
                    tid: 0,
                    fbit: true,
                    ..Default::default()
                },
                // TRAIL_NUT (type 0), tid 1
                TestType {
                    raw_header: &[0x00, 0x01],
                    typ: 0,
                    layer_id: 0,
                    tid: 1,
                    ..Default::default()
                },
                // VPS_NUT (type 14)
                TestType {
                    raw_header: &[0x00, 0x71],
                    typ: 14,
                    layer_id: 0,
                    tid: 1,
                    ..Default::default()
                },
                // SPS_NUT (type 15)
                TestType {
                    raw_header: &[0x00, 0x79],
                    typ: 15,
                    layer_id: 0,
                    tid: 1,
                    ..Default::default()
                },
                // PPS_NUT (type 16)
                TestType {
                    raw_header: &[0x00, 0x81],
                    typ: 16,
                    layer_id: 0,
                    tid: 1,
                    ..Default::default()
                },
                // PREFIX_SEI_NUT (type 23)
                TestType {
                    raw_header: &[0x00, 0xB9],
                    typ: 23,
                    layer_id: 0,
                    tid: 1,
                    ..Default::default()
                },
                // Fragmentation Unit (type 29)
                TestType {
                    raw_header: &[0x00, 0xE9],
                    typ: H266NALU_FRAGMENTATION_UNIT_TYPE,
                    layer_id: 0,
                    tid: 1,
                    is_fu: true,
                    ..Default::default()
                },
                // Aggregation Packet (type 28)
                TestType {
                    raw_header: &[0x00, 0xE1],
                    typ: H266NALU_AGGREGATION_PACKET_TYPE,
                    layer_id: 0,
                    tid: 1,
                    is_ap: true,
                    ..Default::default()
                },
                // layer_id = 5 (byte 0 low 6 bits), IDR_W_RADL, tid 1
                TestType {
                    raw_header: &[0x05, 0x39],
                    typ: 7,
                    layer_id: 5,
                    tid: 1,
                    ..Default::default()
                },
                // tid = 3
                TestType {
                    raw_header: &[0x00, 0x0B],
                    typ: 1,
                    layer_id: 0,
                    tid: 3,
                    ..Default::default()
                },
            ];

            for (i, cur) in tests.iter().enumerate() {
                let header = H266NALUHeader::new(cur.raw_header[0], cur.raw_header[1]);

                assert_eq!(header.f(), cur.fbit, "tc {i}: f bit");
                assert_eq!(header.nalu_type(), cur.typ, "tc {i}: type");
                assert_eq!(header.layer_id(), cur.layer_id, "tc {i}: layer_id");
                assert_eq!(header.tid(), cur.tid, "tc {i}: tid");
                assert_eq!(header.is_aggregation_packet(), cur.is_ap, "tc {i}: is_ap");
                assert_eq!(header.is_fragmentation_unit(), cur.is_fu, "tc {i}: is_fu");
            }

            Ok(())
        }

        /// Test IRAP detection across all NAL types.
        /// H.266 IRAP: IDR_W_RADL(7), IDR_N_LP(8), CRA(9). GDR(10) is not.
        #[test]
        fn test_h266_irap_detection() -> Result<()> {
            for typ in 0u8..=31 {
                let header = H266NALUHeader::new_with_type(typ, 0, 1);
                let expected = (7..=9).contains(&typ);
                assert_eq!(
                    header.is_irap(),
                    expected,
                    "type {typ} irap should be {expected}"
                );
            }
            Ok(())
        }

        /// Test FU header bit parsing/serialization including the H.266
        /// specific P (last-FU-of-picture) bit.
        #[test]
        fn test_h266_fu_header() -> Result<()> {
            struct TestType {
                value: u8,
                s: bool,
                e: bool,
                p: bool,
                typ: u8,
            }

            let tests = [
                // S=1, type 7 (IDR_W_RADL)
                TestType {
                    value: 0x87,
                    s: true,
                    e: false,
                    p: false,
                    typ: 7,
                },
                // E=1, type 7
                TestType {
                    value: 0x47,
                    s: false,
                    e: true,
                    p: false,
                    typ: 7,
                },
                // P=1, type 7
                TestType {
                    value: 0x27,
                    s: false,
                    e: false,
                    p: true,
                    typ: 7,
                },
                // E+P, type 0 (TRAIL)
                TestType {
                    value: 0x60,
                    s: false,
                    e: true,
                    p: true,
                    typ: 0,
                },
                // S, type 31 (max 5-bit type)
                TestType {
                    value: 0x9F,
                    s: true,
                    e: false,
                    p: false,
                    typ: 31,
                },
                // nothing set, type 9 (CRA)
                TestType {
                    value: 0x09,
                    s: false,
                    e: false,
                    p: false,
                    typ: 9,
                },
            ];

            // F bit set rejects as corrupted.
            {
                let mut depack = H266Depacketizer::default();
                let mut out = Vec::new();
                let mut extra = CodecExtra::None;
                let res = depack.depacketize(&[0x80, 0x01, 0x93, 0xAF, 0xAF], &mut out, &mut extra);
                assert!(matches!(res, Err(PacketError::ErrH266CorruptedPacket)));
            }

            for (i, t) in tests.iter().enumerate() {
                let fu = H266FragmentationUnitHeader(t.value);
                assert_eq!(fu.s(), t.s, "tc {i}: s");
                assert_eq!(fu.e(), t.e, "tc {i}: e");
                assert_eq!(fu.p(), t.p, "tc {i}: p");
                assert_eq!(fu.fu_type(), t.typ, "tc {i}: type");

                // round-trip via constructor (p only via raw value)
                let built = H266FragmentationUnitHeader::new(t.s, t.e, t.p, t.typ);
                assert_eq!(built.0, t.value, "tc {i}: build");
            }

            Ok(())
        }
    }

    /// Depacketizer behavior tests, expressed against the depacketize() API.
    mod depacketizer_tests {
        use super::*;

        /// Valid/invalid single NAL unit packets.
        #[test]
        fn test_h266_single_nalunit_packet() -> Result<()> {
            // Valid single NAL (TRAIL, type 0) -> Annex-B out.
            let nal = make_nal(0, 5);
            let (out, keyframe) = depacketize_all(std::slice::from_ref(&nal))?;
            assert_eq!(&out[..4], &[0, 0, 0, 1]);
            assert_eq!(&out[4..], &nal[..]);
            assert!(!keyframe);

            // IDR single NAL flags keyframe.
            let idr = make_nal(H266NALU_IDR_W_RADL, 5);
            let (out, keyframe) = depacketize_all(std::slice::from_ref(&idr))?;
            assert_eq!(&out[4..], &idr[..]);
            assert!(keyframe);

            // Too short (header only, no payload beyond 2 bytes).
            let mut depack = H266Depacketizer::default();
            let mut out = Vec::new();
            let mut extra = CodecExtra::None;
            let err = depack.depacketize(&hdr(0, 0, 1), &mut out, &mut extra);
            assert_eq!(err, Err(PacketError::ErrShortPacket));

            // Empty packet.
            let err = depack.depacketize(&[], &mut out, &mut extra);
            assert_eq!(err, Err(PacketError::ErrShortPacket));

            // F bit set is rejected as corrupted.
            let err = depack.depacketize(&[0x80, 0x01, 0xAA, 0xBB], &mut out, &mut extra);
            assert!(matches!(err, Err(PacketError::ErrH266CorruptedPacket)));

            Ok(())
        }

        /// Valid/invalid aggregation packets.
        #[test]
        fn test_h266_aggregation_packet() -> Result<()> {
            let sps = make_nal(H266NALU_SPS_NALU_TYPE, 6);
            let pps = make_nal(H266NALU_PPS_NALU_TYPE, 4);

            // Build AP: [AP hdr][len][sps][len][pps]
            let mut ap = hdr(H266NALU_AGGREGATION_PACKET_TYPE, 0, 1).to_vec();
            ap.extend_from_slice(&(sps.len() as u16).to_be_bytes());
            ap.extend_from_slice(&sps);
            ap.extend_from_slice(&(pps.len() as u16).to_be_bytes());
            ap.extend_from_slice(&pps);

            let (out, keyframe) = depacketize_all(&[ap])?;
            let expected = annexb(&[&sps, &pps]);
            assert_eq!(out, expected);
            assert!(!keyframe);

            // AP with a single unit violates RFC 9328 (>= 2 required).
            let mut ap1 = hdr(H266NALU_AGGREGATION_PACKET_TYPE, 0, 1).to_vec();
            ap1.extend_from_slice(&(sps.len() as u16).to_be_bytes());
            ap1.extend_from_slice(&sps);
            let mut depack = H266Depacketizer::default();
            let mut out = Vec::new();
            let mut extra = CodecExtra::None;
            let err = depack.depacketize(&ap1, &mut out, &mut extra);
            assert_eq!(err, Err(PacketError::ErrShortPacket));

            // Truncated size field: claims more bytes than present.
            let mut bad = hdr(H266NALU_AGGREGATION_PACKET_TYPE, 0, 1).to_vec();
            bad.extend_from_slice(&[0x00, 0x20]); // size 32 but nothing follows
            let err = depack.depacketize(&bad, &mut out, &mut extra);
            assert_eq!(err, Err(PacketError::ErrShortPacket));

            // AP containing an IRAP flags keyframe.
            let idr = make_nal(H266NALU_IDR_N_LP, 6);
            let mut ap_idr = hdr(H266NALU_AGGREGATION_PACKET_TYPE, 0, 1).to_vec();
            ap_idr.extend_from_slice(&(sps.len() as u16).to_be_bytes());
            ap_idr.extend_from_slice(&sps);
            ap_idr.extend_from_slice(&(idr.len() as u16).to_be_bytes());
            ap_idr.extend_from_slice(&idr);
            let (_, keyframe) = depacketize_all(&[ap_idr])?;
            assert!(keyframe);

            Ok(())
        }

        /// FU reassembly: start/middle/end, header reconstruction, orphan
        /// fragments.
        #[test]
        fn test_h266_fragmentation_unit_packet() -> Result<()> {
            // Original NAL: CRA (type 9), 8 bytes.
            let orig = make_nal(H266NALU_CRA, 8);
            let fu_hdr_bytes = hdr(H266NALU_FRAGMENTATION_UNIT_TYPE, 0, 1);

            // Fragment payload (after the original 2-byte header).
            let payload = &orig[2..];
            let (a, b) = payload.split_at(payload.len() / 2);

            let mut fu1 = fu_hdr_bytes.to_vec();
            fu1.push(H266FragmentationUnitHeader::new(true, false, false, H266NALU_CRA).0);
            fu1.extend_from_slice(a);

            let mut fu2 = fu_hdr_bytes.to_vec();
            fu2.push(H266FragmentationUnitHeader::new(false, true, false, H266NALU_CRA).0);
            fu2.extend_from_slice(b);

            let (out, keyframe) = depacketize_all(&[fu1, fu2])?;
            assert_eq!(out, annexb(&[&orig]));
            assert!(keyframe, "CRA via FU must flag keyframe");

            // FU packet too short for an FU header.
            let mut depack = H266Depacketizer::default();
            let mut out = Vec::new();
            let mut extra = CodecExtra::None;
            let err = depack.depacketize(&fu_hdr_bytes, &mut out, &mut extra);
            assert_eq!(err, Err(PacketError::ErrShortPacket));

            // Orphan middle/end fragment on a fresh depacketizer: silently
            // consumed, no output.
            let mut orphan = hdr(H266NALU_FRAGMENTATION_UNIT_TYPE, 0, 1).to_vec();
            orphan.push(H266FragmentationUnitHeader::new(false, true, false, 0).0);
            orphan.extend_from_slice(&[0xAA, 0xBB]);
            let mut depack = H266Depacketizer::default();
            let mut out = Vec::new();
            depack.depacketize(&orphan, &mut out, &mut extra)?;
            assert!(out.is_empty(), "orphan FU must not produce output");

            Ok(())
        }

        /// General depacketizer error/dispatch table.
        #[test]
        fn test_h266_packet() -> Result<()> {
            struct TestType {
                raw: &'static [u8],
                expect_err: bool, // all error cases expect ErrShortPacket
            }

            let tests = [
                // empty
                TestType {
                    raw: &[],
                    expect_err: true,
                },
                // header only
                TestType {
                    raw: &[0x00, 0x01],
                    expect_err: true,
                },
                // valid single NAL (TRAIL)
                TestType {
                    raw: &[0x00, 0x01, 0xAB, 0xCD, 0xEF],
                    expect_err: false,
                },
                // FU too short (no FU header)
                TestType {
                    raw: &[0x00, 0xE9],
                    expect_err: true,
                },
                // valid FU start fragment (S=1, type 7)
                TestType {
                    raw: &[0x00, 0xE9, 0x87, 0x11, 0x22],
                    expect_err: false,
                },
                // AP with one unit only -> error
                TestType {
                    raw: &[0x00, 0xE1, 0x00, 0x03, 0x00, 0x01, 0xAA],
                    expect_err: true,
                },
                // valid AP with two units
                TestType {
                    raw: &[
                        0x00, 0xE1, // AP header
                        0x00, 0x03, 0x00, 0x01, 0xAA, // unit 1 (TRAIL)
                        0x00, 0x03, 0x00, 0x01, 0xBB, // unit 2 (TRAIL)
                    ],
                    expect_err: false,
                },
            ];

            for (i, t) in tests.iter().enumerate() {
                let mut depack = H266Depacketizer::default();
                let mut out = Vec::new();
                let mut extra = CodecExtra::None;
                let res = depack.depacketize(t.raw, &mut out, &mut extra);
                if t.expect_err {
                    assert!(
                        matches!(res, Err(PacketError::ErrShortPacket)),
                        "tc {i} should error, got {res:?}"
                    );
                } else {
                    assert!(res.is_ok(), "tc {i} should succeed: {res:?}");
                }
            }

            Ok(())
        }
    }

    /// Packetizer behavior tests.
    mod packetizer_tests {
        use super::*;

        /// A payload without start codes is treated as one NAL unit.
        #[test]
        fn test_h266_packetizer_single_nalu() -> Result<()> {
            let mut pck = H266Packetizer::default();
            let nal = make_nal(0, 10);
            let packets = pck.packetize(1200, &nal)?;
            assert_eq!(packets.len(), 1);
            assert_eq!(packets[0], nal);
            Ok(())
        }

        /// Annex-B input is split into one packet per NAL.
        #[test]
        fn test_h266_packetizer_annexb_split() -> Result<()> {
            let n1 = make_nal(0, 6);
            let n2 = make_nal(1, 7);
            // mix 4-byte and 3-byte start codes
            let mut payload = Vec::new();
            payload.extend_from_slice(&[0, 0, 0, 1]);
            payload.extend_from_slice(&n1);
            payload.extend_from_slice(&[0, 0, 1]);
            payload.extend_from_slice(&n2);

            let mut pck = H266Packetizer::default();
            let packets = pck.packetize(1200, &payload)?;
            assert_eq!(packets.len(), 2);
            assert_eq!(packets[0], n1);
            assert_eq!(packets[1], n2);
            Ok(())
        }

        /// NAL exactly == mtu stays a single packet (no fragmentation).
        #[test]
        fn test_h266_packetizer_single_nalu_no_fragment() -> Result<()> {
            let mut pck = H266Packetizer::default();
            let nal = make_nal(0, 100);
            let packets = pck.packetize(100, &nal)?;
            assert_eq!(packets.len(), 1);
            assert_eq!(packets[0], nal);
            Ok(())
        }

        /// Large NAL is fragmented into FUs and can be reconstructed.
        #[test]
        fn test_h266_packetizer_fu_fragmentation_roundtrip_payload() -> Result<()> {
            let mut pck = H266Packetizer::default();
            let nal = make_nal(H266NALU_CRA, 3000);
            let mtu = 200;
            let packets = pck.packetize(mtu, &nal)?;
            assert!(packets.len() > 1);
            for p in &packets {
                assert!(p.len() <= mtu);
                let h = H266NALUHeader::new(p[0], p[1]);
                assert!(h.is_fragmentation_unit());
            }
            let rebuilt = reconstruct_from_fu_packets(&packets);
            assert_eq!(rebuilt, nal);
            Ok(())
        }

        /// VPS+SPS+PPS are aggregated into one AP before the next NAL.
        #[test]
        fn test_h266_packetizer_emits_ap_for_vps_sps_pps() -> Result<()> {
            let vps = make_nal(H266NALU_VPS_NALU_TYPE, 8);
            let sps = make_nal(H266NALU_SPS_NALU_TYPE, 12);
            let pps = make_nal(H266NALU_PPS_NALU_TYPE, 6);
            let idr = make_nal(H266NALU_IDR_W_RADL, 20);
            let payload = annexb(&[&vps, &sps, &pps, &idr]);

            let mut pck = H266Packetizer::default();
            let packets = pck.packetize(1200, &payload)?;
            assert_eq!(packets.len(), 2, "AP + IDR single expected");

            // packets[0] is the AP with 3 units.
            let ap = &packets[0];
            let h = H266NALUHeader::new(ap[0], ap[1]);
            assert!(h.is_aggregation_packet());
            let mut units = Vec::new();
            let mut off = 2;
            while off + 2 <= ap.len() {
                let sz = u16::from_be_bytes([ap[off], ap[off + 1]]) as usize;
                off += 2;
                units.push(ap[off..off + sz].to_vec());
                off += sz;
            }
            assert_eq!(units.len(), 3);
            assert_eq!(units[0], vps);
            assert_eq!(units[1], sps);
            assert_eq!(units[2], pps);

            // packets[1] is the IDR single NAL.
            assert_eq!(packets[1], idr);
            Ok(())
        }

        /// When the AP would exceed the MTU, parameter sets fall back to
        /// single NAL packets.
        #[test]
        fn test_h266_packetizer_ap_exceeds_mtu_fallback() -> Result<()> {
            let mtu = 700;
            let sps = make_nal(H266NALU_SPS_NALU_TYPE, 600);
            let pps = make_nal(H266NALU_PPS_NALU_TYPE, 600);
            let idr = make_nal(H266NALU_IDR_W_RADL, 50);
            let payload = annexb(&[&sps, &pps, &idr]);

            let mut pck = H266Packetizer::default();
            let packets = pck.packetize(mtu, &payload)?;

            // AP would be 2 + (2+600)*2 = 1206 > 700 -> fallback singles.
            assert_eq!(packets.len(), 3);
            assert_eq!(packets[0], sps);
            assert_eq!(packets[1], pps);
            assert_eq!(packets[2], idr);
            Ok(())
        }

        /// AUD NALs are dropped by the packetizer, filler NALs are kept.
        #[test]
        fn test_h266_packetizer_drops_aud_keeps_filler() -> Result<()> {
            let aud = make_nal(H266NALU_AUD_NALU_TYPE, 4);
            let fd = make_nal(H266NALU_FILLER_NALU_TYPE, 10);
            let trail = make_nal(0, 8);
            let payload = annexb(&[&aud, &trail, &fd]);

            let mut pck = H266Packetizer::default();
            let packets = pck.packetize(1200, &payload)?;
            assert_eq!(packets.len(), 2);
            assert_eq!(packets[0], trail);
            assert_eq!(packets[1], fd);
            Ok(())
        }

        /// is_marker: last packet of the AU carries the marker.
        #[test]
        fn test_h266_packetizer_marker() -> Result<()> {
            let mut pck = H266Packetizer::default();
            assert!(pck.is_marker(&[], None, true));
            assert!(!pck.is_marker(&[], None, false));
            Ok(())
        }

        /// FU S/E flags: first fragment S, last fragment E, middles neither.
        /// The P bit is never set (RFC 9328 only mandates P=0 for
        /// non-final fragments; always 0 is compliant).
        #[test]
        fn test_h266_fragmentation_start_end_flags() -> Result<()> {
            let mut pck = H266Packetizer::default();
            let nal = make_nal(0, 1000);
            let packets = pck.packetize(100, &nal)?;
            assert!(packets.len() >= 3);

            for (i, p) in packets.iter().enumerate() {
                let fu = H266FragmentationUnitHeader(p[2]);
                let is_first = i == 0;
                let is_last = i == packets.len() - 1;
                assert_eq!(fu.s(), is_first, "pkt {i} S");
                assert_eq!(fu.e(), is_last, "pkt {i} E");
                assert!(!fu.p(), "pkt {i} P never set");
                assert_eq!(fu.fu_type(), 0, "pkt {i} type");
            }
            Ok(())
        }

        /// Exact boundary: len == mtu single; len == mtu+1 fragments, with
        /// exact fragment sizes.
        #[test]
        fn test_h266_packetizer_exact_fu_boundary_mtu() -> Result<()> {
            let mtu = 100;

            // len == mtu -> single
            let mut pck = H266Packetizer::default();
            let nal = make_nal(0, mtu);
            let packets = pck.packetize(mtu, &nal)?;
            assert_eq!(packets.len(), 1);

            // len == mtu + 1 -> FU; payload 99 bytes; max fragment = mtu - 3 = 97
            let mut pck = H266Packetizer::default();
            let nal = make_nal(0, mtu + 1);
            let packets = pck.packetize(mtu, &nal)?;
            assert_eq!(packets.len(), 2);
            assert_eq!(packets[0].len(), 3 + 97);
            assert_eq!(packets[1].len(), 3 + 2);
            assert_eq!(reconstruct_from_fu_packets(&packets), nal);
            Ok(())
        }

        /// MTU sweep roundtrip.
        #[test]
        fn test_h266_mtu_variation() -> Result<()> {
            for mtu in [50usize, 128, 512, 1200] {
                let mut pck = H266Packetizer::default();
                let nal = make_nal(H266NALU_IDR_N_LP, 1000);
                let packets = pck.packetize(mtu, &nal)?;
                for p in &packets {
                    assert!(p.len() <= mtu, "mtu {mtu}: packet {} too big", p.len());
                }
                if nal.len() <= mtu {
                    assert_eq!(packets.len(), 1);
                } else {
                    assert_eq!(reconstruct_from_fu_packets(&packets), nal, "mtu {mtu}");
                }
            }
            Ok(())
        }

        #[test]
        fn packetize_respects_mtu() -> Result<()> {
            // 2-byte NAL header (non-parameter-set type) + payload.
            let nal = make_nal(0, 2002);
            for &mtu in &[100usize, 300, 600, 1200] {
                let mut pck = H266Packetizer::default();
                let pkts = pck.packetize(mtu, &nal)?;
                assert!(!pkts.is_empty(), "H266 produced no packets at mtu {mtu}");
                for (i, pkt) in pkts.iter().enumerate() {
                    assert!(
                        pkt.len() <= mtu,
                        "H266 packet {i} size {} > mtu {mtu}",
                        pkt.len()
                    );
                }
            }
            Ok(())
        }
    }

    /// Packetize → depacketize roundtrips.
    mod roundtrip_tests {
        use super::*;

        /// FU roundtrip.
        #[test]
        fn test_h266_fu_roundtrip_with_depacketizer() -> Result<()> {
            let mut pck = H266Packetizer::default();
            let nal = make_nal(H266NALU_IDR_W_RADL, 2500);
            let packets = pck.packetize(400, &nal)?;
            assert!(packets.len() > 1);

            let (out, keyframe) = depacketize_all(&packets)?;
            assert_eq!(out, annexb(&[&nal]));
            assert!(keyframe);
            Ok(())
        }

        /// AP roundtrip.
        #[test]
        fn test_h266_ap_roundtrip_with_depacketizer() -> Result<()> {
            let vps = make_nal(H266NALU_VPS_NALU_TYPE, 8);
            let sps = make_nal(H266NALU_SPS_NALU_TYPE, 12);
            let pps = make_nal(H266NALU_PPS_NALU_TYPE, 6);
            let idr = make_nal(H266NALU_IDR_N_LP, 40);
            let payload = annexb(&[&vps, &sps, &pps, &idr]);

            let mut pck = H266Packetizer::default();
            let packets = pck.packetize(1200, &payload)?;

            let (out, keyframe) = depacketize_all(&packets)?;
            assert_eq!(out, annexb(&[&vps, &sps, &pps, &idr]));
            assert!(keyframe);
            Ok(())
        }

        /// Single NAL roundtrip.
        #[test]
        fn test_h266_single_nalu_roundtrip_with_depacketizer() -> Result<()> {
            let mut pck = H266Packetizer::default();
            let nal = make_nal(0, 50);
            let packets = pck.packetize(1200, &nal)?;
            assert_eq!(packets.len(), 1);

            let (out, keyframe) = depacketize_all(&packets)?;
            assert_eq!(out, annexb(&[&nal]));
            assert!(!keyframe);
            Ok(())
        }

        /// Mixed AU: params (AP) + small (single) + large (FU) NALs.
        #[test]
        fn test_h266_mixed_packet_types_roundtrip() -> Result<()> {
            let sps = make_nal(H266NALU_SPS_NALU_TYPE, 10);
            let pps = make_nal(H266NALU_PPS_NALU_TYPE, 6);
            let small = make_nal(H266NALU_IDR_W_RADL, 100);
            let large = make_nal(0, 1500);
            let payload = annexb(&[&sps, &pps, &small, &large]);

            let mut pck = H266Packetizer::default();
            let mtu = 600;
            let packets = pck.packetize(mtu, &payload)?;

            // AP (sps+pps) + 1 single + >=3 FUs
            assert!(packets.len() >= 5, "got {}", packets.len());
            for p in &packets {
                assert!(p.len() <= mtu);
            }

            let (out, keyframe) = depacketize_all(&packets)?;
            assert_eq!(out, annexb(&[&sps, &pps, &small, &large]));
            assert!(keyframe);
            Ok(())
        }

        /// Multi-NAL Annex-B with mixed start-code lengths.
        #[test]
        fn test_h266_annexb_roundtrip_with_depacketizer() -> Result<()> {
            let n1 = make_nal(1, 30);
            let n2 = make_nal(2, 40);
            let n3 = make_nal(0, 25);

            let mut payload = Vec::new();
            payload.extend_from_slice(&[0, 0, 1]); // 3-byte start code
            payload.extend_from_slice(&n1);
            payload.extend_from_slice(&[0, 0, 0, 1]); // 4-byte
            payload.extend_from_slice(&n2);
            payload.extend_from_slice(&[0, 0, 1]);
            payload.extend_from_slice(&n3);

            let mut pck = H266Packetizer::default();
            let packets = pck.packetize(1200, &payload)?;
            assert_eq!(packets.len(), 3);

            let (out, _) = depacketize_all(&packets)?;
            assert_eq!(out, annexb(&[&n1, &n2, &n3]));
            Ok(())
        }
    }

    /// Partition head/tail classification.
    mod partition_tests {
        use super::*;
        #[test]
        fn test_h266_is_partition_head() -> Result<()> {
            let depack = H266Depacketizer::default();

            // Too short -> false.
            assert!(!depack.is_partition_head(&[0x00]));

            // Single NAL -> head.
            assert!(depack.is_partition_head(&make_nal(0, 5)));

            // AP -> head.
            assert!(depack.is_partition_head(&hdr(H266NALU_AGGREGATION_PACKET_TYPE, 0, 1)));

            // FU with S=1 -> head.
            let mut fu_s = hdr(H266NALU_FRAGMENTATION_UNIT_TYPE, 0, 1).to_vec();
            fu_s.push(H266FragmentationUnitHeader::new(true, false, false, 0).0);
            assert!(depack.is_partition_head(&fu_s));

            // FU middle (no S) -> not head.
            let mut fu_m = hdr(H266NALU_FRAGMENTATION_UNIT_TYPE, 0, 1).to_vec();
            fu_m.push(H266FragmentationUnitHeader::new(false, false, false, 0).0);
            assert!(!depack.is_partition_head(&fu_m));

            // FU header missing -> not head.
            assert!(!depack.is_partition_head(&hdr(H266NALU_FRAGMENTATION_UNIT_TYPE, 0, 1)));

            // F bit -> treated as head (error case).
            assert!(depack.is_partition_head(&[0x80, 0x01, 0x00]));

            Ok(())
        }
        #[test]
        fn test_h266_is_partition_tail() -> Result<()> {
            let depack = H266Depacketizer::default();

            // Too short -> false even with marker.
            assert!(!depack.is_partition_tail(true, &[0x00]));

            // Single NAL: marker decides.
            let single = make_nal(0, 5);
            assert!(depack.is_partition_tail(true, &single));
            assert!(!depack.is_partition_tail(false, &single));

            // FU with E=1 -> tail regardless of marker.
            let mut fu_e = hdr(H266NALU_FRAGMENTATION_UNIT_TYPE, 0, 1).to_vec();
            fu_e.push(H266FragmentationUnitHeader::new(false, true, false, 0).0);
            assert!(depack.is_partition_tail(false, &fu_e));

            // FU without E -> not tail, marker ignored for FU.
            let mut fu_s = hdr(H266NALU_FRAGMENTATION_UNIT_TYPE, 0, 1).to_vec();
            fu_s.push(H266FragmentationUnitHeader::new(true, false, false, 0).0);
            assert!(!depack.is_partition_tail(true, &fu_s));

            // FU header missing -> false.
            assert!(!depack.is_partition_tail(true, &hdr(H266NALU_FRAGMENTATION_UNIT_TYPE, 0, 1)));

            Ok(())
        }
    }

    /// MTU and malformed-input edge cases.
    mod edge_tests {
        use super::*;
        #[test]
        fn test_h266_zero_mtu() -> Result<()> {
            let mut pck = H266Packetizer::default();
            let packets = pck.packetize(0, &make_nal(0, 10))?;
            assert!(packets.is_empty());
            Ok(())
        }
        #[test]
        fn test_h266_empty_nalu() -> Result<()> {
            let mut pck = H266Packetizer::default();
            let packets = pck.packetize(1200, &[])?;
            assert!(packets.is_empty());
            Ok(())
        }

        /// MTU below the minimum (header+fu+1) yields no packets.
        #[test]
        fn test_h266_mtu_smaller_than_fu_overhead() -> Result<()> {
            let mut pck = H266Packetizer::default();
            // MIN_MTU is 4; mtu 3 == FU overhead exactly -> cannot carry payload.
            let packets = pck.packetize(3, &make_nal(0, 100))?;
            assert!(packets.is_empty());
            Ok(())
        }

        /// Minimal viable MTU fragments 1 byte at a time and round-trips.
        #[test]
        fn test_h266_fu_minimal_mtu() -> Result<()> {
            let mut pck = H266Packetizer::default();
            let nal = make_nal(0, 10);
            let packets = pck.packetize(MIN_MTU, &nal)?;
            // payload bytes = 8, one per fragment
            assert_eq!(packets.len(), 8);
            for p in &packets {
                assert_eq!(p.len(), 4);
            }
            assert_eq!(reconstruct_from_fu_packets(&packets), nal);
            Ok(())
        }

        /// MTU above MAX_PACKET_SIZE is clamped.
        #[test]
        fn test_h266_fu_mtu_exceeds_max_packet_size() -> Result<()> {
            let mut pck = H266Packetizer::default();
            let nal = make_nal(0, 3000);
            let packets = pck.packetize(5000, &nal)?;
            assert!(packets.len() > 1, "must fragment despite huge mtu");
            for p in &packets {
                assert!(p.len() <= MAX_PACKET_SIZE);
            }
            assert_eq!(reconstruct_from_fu_packets(&packets), nal);
            Ok(())
        }

        /// A NAL smaller than the 2-byte header is skipped.
        #[test]
        fn test_h266_nalu_smaller_than_header() -> Result<()> {
            let mut pck = H266Packetizer::default();
            // one-byte "NAL" between start codes
            let mut payload = Vec::new();
            payload.extend_from_slice(&[0, 0, 0, 1]);
            payload.push(0x00);
            let packets = pck.packetize(1200, &payload)?;
            assert!(packets.is_empty());
            Ok(())
        }

        /// Exact AP byte layout.
        #[test]
        fn test_h266_aggregated_exact_layout() -> Result<()> {
            let sps = make_nal(H266NALU_SPS_NALU_TYPE, 4);
            let pps = make_nal(H266NALU_PPS_NALU_TYPE, 3);
            let idr = make_nal(H266NALU_IDR_W_RADL, 5);
            let payload = annexb(&[&sps, &pps, &idr]);

            let mut pck = H266Packetizer::default();
            let packets = pck.packetize(1200, &payload)?;
            assert_eq!(packets.len(), 2);

            // Expected AP: [0x00, (28<<3)|tid=1] [00 04 sps] [00 03 pps]
            let mut expected = vec![0x00, (H266NALU_AGGREGATION_PACKET_TYPE << 3) | 1];
            expected.extend_from_slice(&(sps.len() as u16).to_be_bytes());
            expected.extend_from_slice(&sps);
            expected.extend_from_slice(&(pps.len() as u16).to_be_bytes());
            expected.extend_from_slice(&pps);
            assert_eq!(packets[0], expected);
            Ok(())
        }

        /// Synthetic "real-world" AU shape: params + IDR slice, verified
        /// end-to-end (structure synthesized for VVC).
        #[test]
        fn test_h266_packet_synthetic_au() -> Result<()> {
            let sps = make_nal(H266NALU_SPS_NALU_TYPE, 45);
            let pps = make_nal(H266NALU_PPS_NALU_TYPE, 12);
            let aps = make_nal(17, 30); // PREFIX_APS passes through as single
            let idr = make_nal(H266NALU_IDR_N_LP, 800);
            let payload = annexb(&[&sps, &pps, &aps, &idr]);

            let mut pck = H266Packetizer::default();
            let packets = pck.packetize(1200, &payload)?;

            // AP(sps+pps) + APS single + IDR single
            assert_eq!(packets.len(), 3);

            let (out, keyframe) = depacketize_all(&packets)?;
            assert_eq!(out, annexb(&[&sps, &pps, &aps, &idr]));
            assert!(keyframe);
            Ok(())
        }
    }

    /// Keyframe detection on raw RTP payloads.
    #[test]
    fn test_detect_h266_keyframe() {
        // Empty / too short payload
        assert!(!detect_h266_keyframe(&[]));
        assert!(!detect_h266_keyframe(&[0x00]));

        // Single IRAP NALs
        for typ in [H266NALU_IDR_W_RADL, H266NALU_IDR_N_LP, H266NALU_CRA] {
            let h = H266NALUHeader::new_with_type(typ, 0, 1);
            assert!(
                detect_h266_keyframe(&h.0.to_be_bytes()),
                "type {typ} must be keyframe"
            );
        }

        // GDR (type 10) is not treated as IRAP here.
        let gdr = H266NALUHeader::new_with_type(H266NALU_GDR, 0, 1);
        assert!(!detect_h266_keyframe(&gdr.0.to_be_bytes()));

        // TRAIL (type 0) is not a keyframe.
        let trail = H266NALUHeader::new_with_type(0, 0, 1);
        assert!(!detect_h266_keyframe(&trail.0.to_be_bytes()));

        // Aggregation packet with an IDR inside.
        let ap = H266NALUHeader::new_with_type(H266NALU_AGGREGATION_PACKET_TYPE, 0, 1);
        let idr = H266NALUHeader::new_with_type(H266NALU_IDR_W_RADL, 0, 1);
        let mut ap_with_idr = Vec::new();
        ap_with_idr.extend_from_slice(&ap.0.to_be_bytes());
        ap_with_idr.extend_from_slice(&[0x00, 0x03]); // unit size 3
        ap_with_idr.extend_from_slice(&idr.0.to_be_bytes());
        ap_with_idr.push(0x00);
        assert!(detect_h266_keyframe(&ap_with_idr));

        // Aggregation packet without IRAP.
        let non_irap = H266NALUHeader::new_with_type(1, 0, 1);
        let mut ap_no_irap = Vec::new();
        ap_no_irap.extend_from_slice(&ap.0.to_be_bytes());
        ap_no_irap.extend_from_slice(&[0x00, 0x03]);
        ap_no_irap.extend_from_slice(&non_irap.0.to_be_bytes());
        ap_no_irap.push(0x00);
        assert!(!detect_h266_keyframe(&ap_no_irap));

        // FU start fragment with IDR type.
        let fu = H266NALUHeader::new_with_type(H266NALU_FRAGMENTATION_UNIT_TYPE, 0, 1);
        let mut fu_start_idr = Vec::new();
        fu_start_idr.extend_from_slice(&fu.0.to_be_bytes());
        fu_start_idr.push(0x80 | H266NALU_IDR_W_RADL); // S=1, type=7
        fu_start_idr.extend_from_slice(&[0x00, 0x00]);
        assert!(detect_h266_keyframe(&fu_start_idr));

        // FU continuation fragment (S=0) - cannot detect.
        let mut fu_cont = Vec::new();
        fu_cont.extend_from_slice(&fu.0.to_be_bytes());
        fu_cont.push(H266NALU_IDR_W_RADL); // S=0
        fu_cont.extend_from_slice(&[0x00, 0x00]);
        assert!(!detect_h266_keyframe(&fu_cont));

        // FU too short (no FU header byte).
        assert!(!detect_h266_keyframe(&fu.0.to_be_bytes()));
    }

    /// DONL tests (RFC 9328 §4.3, sprop-max-don-diff > 0).
    ///
    mod donl_tests {
        use super::*;
        #[test]
        fn test_h266_donl_single_nal_round_trip() -> Result<()> {
            let mut packetizer = H266Packetizer::default();
            packetizer.with_donl(true);

            let mut depacketizer = H266Depacketizer::default();
            depacketizer.with_donl(true);

            // Single NAL unit (TRAIL, type 1): [0x00, 0x09]
            let nalu = vec![0x00, 0x09, 0xDE, 0xAD, 0xBE, 0xEF];

            let packets = packetizer.packetize(MAX_PACKET_SIZE, &nalu)?;
            assert_eq!(packets.len(), 1, "Should produce 1 packet");

            let packet = &packets[0];

            // Packet structure: [NAL_HDR (2)] [DONL (2)] [PAYLOAD]
            assert!(packet.len() >= 6);
            assert_eq!(packet[0], 0x00);
            assert_eq!(packet[1], 0x09);

            let donl = u16::from_be_bytes([packet[2], packet[3]]);
            assert_eq!(donl, 0, "DONL should be 0 for first NAL");

            assert_eq!(packet[4], 0xDE);
            assert_eq!(packet[5], 0xAD);

            // Depacketize: Annex-B output without DONL.
            let mut out = Vec::new();
            let mut codec_extra = CodecExtra::None;
            depacketizer.depacketize(packet, &mut out, &mut codec_extra)?;

            assert_eq!(out.len(), 10);
            assert_eq!(&out[0..4], ANNEXB_NALUSTART_CODE);
            assert_eq!(&out[4..6], &[0x00, 0x09]);
            assert_eq!(&out[6..10], &[0xDE, 0xAD, 0xBE, 0xEF]);

            Ok(())
        }
        #[test]
        fn test_h266_donl_fragmentation_round_trip() -> Result<()> {
            let mut packetizer = H266Packetizer::default();
            packetizer.with_donl(true);

            let mut depacketizer = H266Depacketizer::default();
            depacketizer.with_donl(true);

            let mut nalu = vec![0x00, 0x09]; // TRAIL (type 1)
            nalu.extend(vec![0xAA; 3000]);

            let packets = packetizer.packetize(1200, &nalu)?;
            assert!(packets.len() >= 3);

            // First FU: [FU_HDR (2)] [FU_HEADER (1)] [DONL (2)] [PAYLOAD]
            let first_packet = &packets[0];
            let fu_header = H266NALUHeader::new(first_packet[0], first_packet[1]);
            assert_eq!(fu_header.nalu_type(), H266NALU_FRAGMENTATION_UNIT_TYPE);

            let fu_hdr = H266FragmentationUnitHeader(first_packet[2]);
            assert!(fu_hdr.s());
            assert!(!fu_hdr.e());

            let donl = u16::from_be_bytes([first_packet[3], first_packet[4]]);
            assert_eq!(donl, 0, "DONL should be 0 for first NAL");

            // Middle fragments: no S/E, payload right after FU header (no DONL).
            if packets.len() > 2 {
                let middle_fu_hdr = H266FragmentationUnitHeader(packets[1][2]);
                assert!(!middle_fu_hdr.s());
                assert!(!middle_fu_hdr.e());
            }

            let last_fu_hdr = H266FragmentationUnitHeader(packets[packets.len() - 1][2]);
            assert!(!last_fu_hdr.s());
            assert!(last_fu_hdr.e());

            // Depacketize all fragments.
            let mut out = Vec::new();
            let mut codec_extra = CodecExtra::None;
            for packet in &packets {
                depacketizer.depacketize(packet, &mut out, &mut codec_extra)?;
            }

            assert_eq!(out.len(), 4 + nalu.len());
            assert_eq!(&out[0..4], ANNEXB_NALUSTART_CODE);
            assert_eq!(&out[4..], &nalu[..]);

            Ok(())
        }
        #[test]
        fn test_h266_donl_aggregation_round_trip() -> Result<()> {
            let mut packetizer = H266Packetizer::default();
            packetizer.with_donl(true);

            let mut depacketizer = H266Depacketizer::default();
            depacketizer.with_donl(true);

            let vps = vec![0x00, 0x71, 0x0C, 0x01, 0xFF, 0xFF]; // VPS (14)
            let sps = vec![0x00, 0x79, 0x01, 0x50, 0x00, 0x00]; // SPS (15)
            let pps = vec![0x00, 0x81, 0xC0, 0xF3, 0xC0, 0x02]; // PPS (16)
            let payload = annexb(&[&vps, &sps, &pps]);

            let packets = packetizer.packetize(MAX_PACKET_SIZE, &payload)?;
            assert_eq!(packets.len(), 0, "Parameter sets should be cached");

            // VCL NAL triggers AP emission.
            let vcl = vec![0x00, 0x09, 0x11, 0x22, 0x33];
            let vcl_packets = packetizer.packetize(MAX_PACKET_SIZE, &annexb(&[&vcl]))?;
            assert_eq!(vcl_packets.len(), 2, "Should produce AP + VCL");

            // AP structure (RFC 9328 §4.3.2):
            // [AP_HDR (2)] [DONL (2)] [SIZE (2)] [NALU] [SIZE (2)] [NALU] ...
            let ap_packet = &vcl_packets[0];
            let ap_header = H266NALUHeader::new(ap_packet[0], ap_packet[1]);
            assert_eq!(ap_header.nalu_type(), H266NALU_AGGREGATION_PACKET_TYPE);

            let donl = u16::from_be_bytes([ap_packet[2], ap_packet[3]]);
            assert_eq!(donl, 0, "DONL should be 0 for first aggregated unit");

            let first_size = u16::from_be_bytes([ap_packet[4], ap_packet[5]]);
            assert_eq!(first_size, 6, "VPS size should be 6 bytes");

            // The second unit's 16-bit size follows the first unit
            // immediately.
            let first_nal_end = 6 + first_size as usize;
            let second_size =
                u16::from_be_bytes([ap_packet[first_nal_end], ap_packet[first_nal_end + 1]]);
            assert_eq!(second_size, 6, "SPS size should be 6 bytes");

            // Depacketize AP: all 3 parameter sets out, DONL stripped.
            let mut out = Vec::new();
            let mut codec_extra = CodecExtra::None;
            depacketizer.depacketize(ap_packet, &mut out, &mut codec_extra)?;

            assert_eq!(out, annexb(&[&vps, &sps, &pps]));

            Ok(())
        }
        #[test]
        fn test_h266_donl_increments_correctly() -> Result<()> {
            let mut packetizer = H266Packetizer::default();
            packetizer.with_donl(true);

            let nalu1 = vec![0x00, 0x09, 0xAA];
            let nalu2 = vec![0x00, 0x09, 0xBB];
            let nalu3 = vec![0x00, 0x09, 0xCC];

            let packets1 = packetizer.packetize(MAX_PACKET_SIZE, &nalu1)?;
            let packets2 = packetizer.packetize(MAX_PACKET_SIZE, &nalu2)?;
            let packets3 = packetizer.packetize(MAX_PACKET_SIZE, &nalu3)?;

            let donl1 = u16::from_be_bytes([packets1[0][2], packets1[0][3]]);
            let donl2 = u16::from_be_bytes([packets2[0][2], packets2[0][3]]);
            let donl3 = u16::from_be_bytes([packets3[0][2], packets3[0][3]]);

            assert_eq!(donl1, 0);
            assert_eq!(donl2, 1);
            assert_eq!(donl3, 2);

            Ok(())
        }
        #[test]
        fn test_h266_without_donl() -> Result<()> {
            let mut packetizer = H266Packetizer::default();
            // DONL disabled by default.

            let nalu = vec![0x00, 0x09, 0xDE, 0xAD, 0xBE, 0xEF];
            let packets = packetizer.packetize(MAX_PACKET_SIZE, &nalu)?;

            assert_eq!(packets.len(), 1);
            assert_eq!(packets[0].len(), nalu.len(), "No DONL field expected");
            assert_eq!(packets[0], nalu);

            Ok(())
        }
        #[test]
        fn test_h266_sdp_driven_donl_all_packet_types() -> Result<()> {
            use crate::format::FormatParams;

            // SDP fmtp with sprop-max-don-diff > 0 enables DONL.
            let fmtp = FormatParams::parse_line("sprop-max-don-diff=32");
            assert_eq!(fmtp.sprop_max_don_diff, Some(32));

            let donl_enabled = fmtp.sprop_max_don_diff.unwrap_or(0) > 0;
            assert!(donl_enabled);

            let mut packetizer = H266Packetizer::default();
            packetizer.with_donl(donl_enabled);

            let mut depacketizer = H266Depacketizer::default();
            depacketizer.with_donl(donl_enabled);

            // --- Test 1: Single NAL with DONL ---
            let single_nalu = vec![0x00, 0x09, 0xDE, 0xAD, 0xBE, 0xEF]; // TRAIL
            let packets = packetizer.packetize(MAX_PACKET_SIZE, &single_nalu)?;
            assert_eq!(packets.len(), 1);

            let pkt = &packets[0];
            assert_eq!(pkt.len(), single_nalu.len() + 2, "2-byte DONL on wire");
            let donl = u16::from_be_bytes([pkt[2], pkt[3]]);
            assert_eq!(donl, 0);

            let mut out = Vec::new();
            let mut extra = CodecExtra::None;
            depacketizer.depacketize(pkt, &mut out, &mut extra)?;
            assert_eq!(&out[0..4], ANNEXB_NALUSTART_CODE);
            assert_eq!(&out[4..], &single_nalu[..]);

            // --- Test 2: AP (VPS + SPS + PPS) with DONL ---
            let vps = vec![0x00, 0x71, 0xAA, 0xBB, 0xCC];
            let sps = vec![0x00, 0x79, 0xDD, 0xEE, 0xFF, 0x11];
            let pps = vec![0x00, 0x81, 0x22, 0x33];
            let vcl = vec![0x00, 0x39, 0x44, 0x55, 0x66]; // IDR_W_RADL (7)

            assert!(packetizer.packetize(MAX_PACKET_SIZE, &vps)?.is_empty());
            assert!(packetizer.packetize(MAX_PACKET_SIZE, &sps)?.is_empty());
            assert!(packetizer.packetize(MAX_PACKET_SIZE, &pps)?.is_empty());

            let ap_packets = packetizer.packetize(MAX_PACKET_SIZE, &vcl)?;
            assert_eq!(ap_packets.len(), 2, "Should produce AP + VCL");

            let ap = &ap_packets[0];
            let ap_hdr = H266NALUHeader::new(ap[0], ap[1]);
            assert_eq!(ap_hdr.nalu_type(), H266NALU_AGGREGATION_PACKET_TYPE);
            let ap_donl = u16::from_be_bytes([ap[2], ap[3]]);
            assert_eq!(ap_donl, 1, "AP DONL should be 1 (single consumed DON=0)");

            let first_size = u16::from_be_bytes([ap[4], ap[5]]) as usize;
            // The next unit's 16-bit size follows the first unit directly.
            let second_size_off = 6 + first_size;
            let second_size =
                u16::from_be_bytes([ap[second_size_off], ap[second_size_off + 1]]) as usize;
            assert_eq!(second_size, 6, "SPS unit size expected");

            out.clear();
            extra = CodecExtra::None;
            depacketizer.depacketize(ap, &mut out, &mut extra)?;

            let mut offset = 0;
            for expected in [&vps, &sps, &pps] {
                assert_eq!(&out[offset..offset + 4], ANNEXB_NALUSTART_CODE);
                offset += 4;
                assert_eq!(&out[offset..offset + expected.len()], &expected[..]);
                offset += expected.len();
            }
            assert_eq!(offset, out.len());

            // VCL DONL after AP consumed DONs 1..=3.
            let vcl_pkt = &ap_packets[1];
            let vcl_donl = u16::from_be_bytes([vcl_pkt[2], vcl_pkt[3]]);
            assert_eq!(vcl_donl, 4);

            out.clear();
            extra = CodecExtra::None;
            depacketizer.depacketize(vcl_pkt, &mut out, &mut extra)?;
            assert_eq!(&out[4..], &vcl[..]);

            // --- Test 3: FU with DONL ---
            let mut large_nalu = vec![0x00, 0x09];
            large_nalu.extend(vec![0xAA; 200]);

            let fu_packets = packetizer.packetize(100, &large_nalu)?;
            assert!(fu_packets.len() > 1);

            let first_fu = &fu_packets[0];
            let fu_hdr_byte = H266FragmentationUnitHeader(first_fu[2]);
            assert!(fu_hdr_byte.s());
            let fu_donl = u16::from_be_bytes([first_fu[3], first_fu[4]]);
            assert_eq!(fu_donl, 5, "FU DONL after VCL consumed DON=4");

            let last_fu = &fu_packets[fu_packets.len() - 1];
            let last_fu_hdr = H266FragmentationUnitHeader(last_fu[2]);
            assert!(last_fu_hdr.e());
            assert!(!last_fu_hdr.s());

            out.clear();
            extra = CodecExtra::None;
            for pkt in &fu_packets {
                depacketizer.depacketize(pkt, &mut out, &mut extra)?;
            }
            assert_eq!(&out[0..4], ANNEXB_NALUSTART_CODE);
            assert_eq!(&out[4..], &large_nalu[..]);

            // --- DONL counter progression ---
            // DON 0 (single), 1-3 (AP), 4 (VCL), 5 (FU) -> next = 6.
            let next_nalu = vec![0x00, 0x09, 0x77];
            let next_packets = packetizer.packetize(MAX_PACKET_SIZE, &next_nalu)?;
            let next_donl = u16::from_be_bytes([next_packets[0][2], next_packets[0][3]]);
            assert_eq!(next_donl, 6);

            Ok(())
        }
        #[test]
        fn test_h266_roundtrip_with_donl() -> Result<()> {
            let mut packetizer = H266Packetizer::default();
            packetizer.with_donl(true);

            let mut depacketizer = H266Depacketizer::default();
            depacketizer.with_donl(true);

            // Single NAL with DONL.
            let single_nalu = vec![0x00, 0x09, 0xFF, 0xFF, 0xFF];
            let packets = packetizer.packetize(MAX_PACKET_SIZE, &single_nalu)?;
            assert_eq!(packets.len(), 1);
            let donl = u16::from_be_bytes([packets[0][2], packets[0][3]]);
            assert_eq!(donl, 0);

            let mut out = Vec::new();
            let mut codec_extra = CodecExtra::None;
            depacketizer.depacketize(&packets[0], &mut out, &mut codec_extra)?;
            assert_eq!(&out[4..], &single_nalu[..]);

            // Fragmented NAL with DONL.
            let mut large_nalu = vec![0x00, 0x09];
            for i in 0..512 {
                large_nalu.push((i % 256) as u8);
            }

            let fu_packets = packetizer.packetize(100, &large_nalu)?;
            assert!(fu_packets.len() > 1);

            let first_donl = u16::from_be_bytes([fu_packets[0][3], fu_packets[0][4]]);
            assert_eq!(first_donl, 1, "Second NAL should have DONL=1");

            out.clear();
            codec_extra = CodecExtra::None;
            for packet in &fu_packets {
                depacketizer.depacketize(packet, &mut out, &mut codec_extra)?;
            }
            assert_eq!(&out[0..4], ANNEXB_NALUSTART_CODE);
            assert_eq!(&out[4..], &large_nalu[..]);

            Ok(())
        }
        #[test]
        fn test_h266_aggregation_with_donl_sequences() -> Result<()> {
            let mut packetizer = H266Packetizer::default();
            packetizer.with_donl(true);

            let vps = vec![0x00, 0x71, 0x00, 0x01, 0x02, 0x03];
            let sps = vec![0x00, 0x79, 0x00, 0x01, 0x02, 0x03];
            let pps = vec![0x00, 0x81, 0x00, 0x01, 0x02, 0x03];

            packetizer.packetize(MAX_PACKET_SIZE, &vps)?;
            packetizer.packetize(MAX_PACKET_SIZE, &sps)?;
            packetizer.packetize(MAX_PACKET_SIZE, &pps)?;

            let vcl = vec![0x00, 0x09, 0xAA, 0xBB];
            let packets = packetizer.packetize(MAX_PACKET_SIZE, &vcl)?;

            assert!(!packets.is_empty());
            for packet in &packets {
                assert!(packet.len() >= 2);
            }

            Ok(())
        }
        #[test]
        fn test_h266_mtu_equals_fu_overhead_plus_donl() -> Result<()> {
            let mut packetizer = H266Packetizer::default();
            packetizer.with_donl(true);

            let mut large_nalu = vec![0x00, 0x09];
            large_nalu.extend(vec![0xCC; 200]);

            // FU overhead (3) + DONL (2) = 5 bytes, no room for payload.
            let packets = packetizer.packetize(5, &large_nalu)?;
            assert!(packets.is_empty());

            Ok(())
        }

        /// Large single NAL with DONL: the DONL bytes count against the
        /// MTU, so NAL + DONL == MTU is the single-NAL limit and one byte
        /// more falls back to fragmentation.
        #[test]
        fn test_h266_single_nal_with_donl_large() -> Result<()> {
            let mut packetizer = H266Packetizer::default();
            packetizer.with_donl(true);

            // NAL + DONL == MTU exactly -> single packet.
            let mut large_nalu = vec![0x00, 0x09];
            large_nalu.extend(vec![0xEE; 1196]); // total 1198 + 2 DONL = 1200
            let packets = packetizer.packetize(MAX_PACKET_SIZE, &large_nalu)?;
            assert_eq!(packets.len(), 1);
            assert_eq!(packets[0].len(), 1200);

            // One byte more -> must fragment (never exceed MTU on the wire).
            let mut packetizer = H266Packetizer::default();
            packetizer.with_donl(true);
            let mut larger_nalu = vec![0x00, 0x09];
            larger_nalu.extend(vec![0xEE; 1197]); // total 1199 + 2 DONL = 1201
            let packets = packetizer.packetize(MAX_PACKET_SIZE, &larger_nalu)?;
            assert!(packets.len() > 1, "must fragment instead of exceeding MTU");
            for p in &packets {
                assert!(p.len() <= MAX_PACKET_SIZE);
            }

            Ok(())
        }
    }

    /// Scalability-related tests: VVC temporal sublayers (TID) and
    /// multilayer streams (nuh_layer_id). The RTP layer's responsibility
    /// is to PRESERVE these fields through fragmentation/aggregation and
    /// to derive AP header fields per RFC 9328 §4.3.2 (lowest LayerId/TID
    /// of the aggregated units). Layer SELECTION (SFU dropping sublayers)
    /// is application policy built on top of these fields.
    mod svc_tests {
        use super::*;

        /// FU fragmentation must preserve LayerId and TID end-to-end:
        /// the FU payload header copies byte 0 (F/Z/LayerId) verbatim and
        /// keeps the TID bits of byte 1; reassembly restores the original
        /// two-byte header exactly.
        #[test]
        fn test_h266_fu_preserves_layer_and_tid() -> Result<()> {
            // TRAIL (type 1) on layer 5, temporal sublayer tid=3.
            let mut nal = hdr(1, 5, 3).to_vec();
            nal.extend(std::iter::repeat_n(0xAB, 998));

            let mut pck = H266Packetizer::default();
            let packets = pck.packetize(100, &nal)?;
            assert!(packets.len() > 1);

            for p in &packets {
                let h = H266NALUHeader::new(p[0], p[1]);
                assert!(h.is_fragmentation_unit());
                assert_eq!(h.layer_id(), 5, "FU header must keep LayerId");
                assert_eq!(h.tid(), 3, "FU header must keep TID");
            }

            // Reassembly restores the exact original header bytes.
            assert_eq!(reconstruct_from_fu_packets(&packets), nal);

            let (out, _) = depacketize_all(&packets)?;
            assert_eq!(out, annexb(&[&nal]));
            Ok(())
        }

        /// The Z bit (nuh_reserved_zero_bit) and F bit live in byte 0,
        /// which FU packets copy verbatim — a stream with Z set survives
        /// the FU round-trip bit-exactly.
        #[test]
        fn test_h266_fu_preserves_z_bit() -> Result<()> {
            let mut nal = hdr(1, 2, 1).to_vec();
            nal[0] |= 0b0100_0000; // set Z
            nal.extend(std::iter::repeat_n(0xCD, 500));

            let mut pck = H266Packetizer::default();
            let packets = pck.packetize(100, &nal)?;
            assert!(packets.len() > 1);

            for p in &packets {
                let h = H266NALUHeader::new(p[0], p[1]);
                assert!(h.z(), "Z bit must be preserved in FU headers");
            }
            assert_eq!(reconstruct_from_fu_packets(&packets), nal);
            Ok(())
        }

        /// RFC 9328 §4.3.2: the AP PayloadHdr LayerId and TID MUST be the
        /// LOWEST LayerId/TID of all aggregated NAL units — and must not
        /// leak from the NAL unit that triggers the AP emission.
        #[test]
        fn test_h266_ap_header_uses_lowest_layer_and_tid() -> Result<()> {
            let vps = make_nal_lt(H266NALU_VPS_NALU_TYPE, 8, 2, 3);
            let sps = make_nal_lt(H266NALU_SPS_NALU_TYPE, 8, 4, 1);
            let pps = make_nal_lt(H266NALU_PPS_NALU_TYPE, 8, 0, 2);
            // Trigger NAL with HIGH layer/tid — must not influence the AP.
            let idr = make_nal_lt(H266NALU_IDR_W_RADL, 20, 9, 7);
            let payload = annexb(&[&vps, &sps, &pps, &idr]);

            let mut pck = H266Packetizer::default();
            let packets = pck.packetize(1200, &payload)?;
            assert_eq!(packets.len(), 2);

            let ap = H266NALUHeader::new(packets[0][0], packets[0][1]);
            assert!(ap.is_aggregation_packet());
            assert_eq!(
                ap.layer_id(),
                0,
                "lowest LayerId of units (0), not the trigger's (9)"
            );
            assert_eq!(
                ap.tid(),
                1,
                "lowest TID of units (1), not the trigger's (7)"
            );
            assert!(!ap.f());
            Ok(())
        }

        /// Aggregated units inside the AP keep their own original headers —
        /// depacketizing yields every unit bit-exact, mixed layers/TIDs
        /// included.
        #[test]
        fn test_h266_ap_units_preserve_individual_headers() -> Result<()> {
            let vps = make_nal_lt(H266NALU_VPS_NALU_TYPE, 8, 2, 3);
            let sps = make_nal_lt(H266NALU_SPS_NALU_TYPE, 8, 4, 1);
            let pps = make_nal_lt(H266NALU_PPS_NALU_TYPE, 8, 0, 2);
            let idr = make_nal_lt(H266NALU_IDR_W_RADL, 20, 1, 1);
            let payload = annexb(&[&vps, &sps, &pps, &idr]);

            let mut pck = H266Packetizer::default();
            let packets = pck.packetize(1200, &payload)?;

            let (out, keyframe) = depacketize_all(&packets)?;
            assert_eq!(out, annexb(&[&vps, &sps, &pps, &idr]));
            assert!(keyframe);
            Ok(())
        }

        /// Keyframe detection is layer/TID-agnostic: an IRAP on a higher
        /// layer or temporal sublayer is still a keyframe — as a single
        /// NAL packet, inside an AP, and as the first FU fragment.
        #[test]
        fn test_h266_keyframe_detection_with_layers() -> Result<()> {
            // Single NAL: CRA on layer 3, tid 2.
            let cra = hdr(H266NALU_CRA, 3, 2);
            assert!(detect_h266_keyframe(&cra));

            // AP containing it.
            let mut ap = hdr(H266NALU_AGGREGATION_PACKET_TYPE, 0, 1).to_vec();
            ap.extend_from_slice(&[0x00, 0x03]);
            ap.extend_from_slice(&cra);
            ap.push(0x00);
            assert!(detect_h266_keyframe(&ap));

            // FU start fragment of it (FU header on layer 3, tid 2).
            let mut fu = hdr(H266NALU_FRAGMENTATION_UNIT_TYPE, 3, 2).to_vec();
            fu.push(0x80 | H266NALU_CRA); // S=1
            fu.extend_from_slice(&[0x00, 0x00]);
            assert!(detect_h266_keyframe(&fu));

            // Non-IRAP on the same layer/tid is not a keyframe.
            assert!(!detect_h266_keyframe(&hdr(1, 3, 2)));
            Ok(())
        }

        /// A full mixed-AU round-trip with differing TIDs per NAL —
        /// e.g. a temporal-base IDR plus a higher-sublayer TRAIL — comes
        /// out bit-exact, so an SFU can still read layer_id()/tid() on the
        /// receive side to implement sublayer dropping.
        #[test]
        fn test_h266_mixed_temporal_layers_roundtrip() -> Result<()> {
            let idr = make_nal_lt(H266NALU_IDR_N_LP, 600, 0, 1); // base layer
            let trail_t2 = make_nal_lt(1, 700, 0, 2); // sublayer 2
            let trail_t3 = make_nal_lt(1, 50, 0, 3); // sublayer 3 (small)
            let payload = annexb(&[&idr, &trail_t2, &trail_t3]);

            let mut pck = H266Packetizer::default();
            let packets = pck.packetize(400, &payload)?;

            let (out, keyframe) = depacketize_all(&packets)?;
            assert_eq!(out, annexb(&[&idr, &trail_t2, &trail_t3]));
            assert!(keyframe);

            // Receive side can classify each NAL by TID again.
            let mut tids = Vec::new();
            let mut off = 0;
            while off + 6 <= out.len() {
                assert_eq!(&out[off..off + 4], &[0, 0, 0, 1]);
                let h = H266NALUHeader::new(out[off + 4], out[off + 5]);
                tids.push(h.tid());
                // skip to next start code
                let mut next = off + 4;
                loop {
                    next += 1;
                    if next + 4 > out.len() {
                        next = out.len();
                        break;
                    }
                    if out[next..next + 4] == [0, 0, 0, 1] {
                        break;
                    }
                }
                off = next;
            }
            assert_eq!(tids, vec![1, 2, 3]);
            Ok(())
        }
    }

    /// Adversarial and malformed-input hardening tests for the AP and FU
    /// paths (header-bit derivation rules, nested RTP-only NAL types,
    /// inconsistent length fields).
    mod hardening_tests {
        use super::*;

        /// the AP header F bit is the OR
        /// of the aggregated units' F bits.
        #[test]
        fn test_h266_ap_header_f_bit_or() -> Result<()> {
            let vps = make_nal(H266NALU_VPS_NALU_TYPE, 6);
            let mut sps = make_nal(H266NALU_SPS_NALU_TYPE, 6);
            sps[0] |= 0b1000_0000; // F set on one unit
            let pps = make_nal(H266NALU_PPS_NALU_TYPE, 6);
            let idr = make_nal(H266NALU_IDR_W_RADL, 10);
            let payload = annexb(&[&vps, &sps, &pps, &idr]);

            let mut pck = H266Packetizer::default();
            let packets = pck.packetize(1200, &payload)?;
            assert_eq!(packets.len(), 2);

            let ap = H266NALUHeader::new(packets[0][0], packets[0][1]);
            assert!(ap.is_aggregation_packet());
            assert!(ap.f(), "AP header F bit must be OR of unit F bits");
            Ok(())
        }

        /// the Z bit of aggregated units
        /// is ignored — the AP header Z is always 0.
        #[test]
        fn test_h266_ap_header_z_bit_ignored() -> Result<()> {
            let mut vps = make_nal(H266NALU_VPS_NALU_TYPE, 6);
            let mut sps = make_nal(H266NALU_SPS_NALU_TYPE, 6);
            let mut pps = make_nal(H266NALU_PPS_NALU_TYPE, 6);
            for n in [&mut vps, &mut sps, &mut pps] {
                n[0] |= 0b0100_0000; // Z set on every unit
            }
            let idr = make_nal(H266NALU_IDR_W_RADL, 10);
            let payload = annexb(&[&vps, &sps, &pps, &idr]);

            let mut pck = H266Packetizer::default();
            let packets = pck.packetize(1200, &payload)?;
            assert_eq!(packets.len(), 2);

            let ap = H266NALUHeader::new(packets[0][0], packets[0][1]);
            assert!(ap.is_aggregation_packet());
            assert!(!ap.z(), "AP header Z bit must be 0 regardless of units");
            Ok(())
        }

        /// an AP containing a nested
        /// AP (type 28) is corrupted.
        #[test]
        fn test_h266_ap_rejects_nested_ap() -> Result<()> {
            let trail = make_nal(0, 4);
            let nested = make_nal(H266NALU_AGGREGATION_PACKET_TYPE, 4);

            let mut ap = hdr(H266NALU_AGGREGATION_PACKET_TYPE, 0, 1).to_vec();
            for unit in [&trail, &nested] {
                ap.extend_from_slice(&(unit.len() as u16).to_be_bytes());
                ap.extend_from_slice(unit);
            }

            let mut depack = H266Depacketizer::default();
            let mut out = Vec::new();
            let mut extra = CodecExtra::None;
            let res = depack.depacketize(&ap, &mut out, &mut extra);
            assert!(matches!(res, Err(PacketError::ErrH266CorruptedPacket)));
            assert!(out.is_empty(), "no partial output on corrupted AP");
            Ok(())
        }

        /// an AP containing a nested
        /// FU (type 29) is corrupted.
        #[test]
        fn test_h266_ap_rejects_nested_fu() -> Result<()> {
            let trail = make_nal(0, 4);
            let nested = make_nal(H266NALU_FRAGMENTATION_UNIT_TYPE, 4);

            let mut ap = hdr(H266NALU_AGGREGATION_PACKET_TYPE, 0, 1).to_vec();
            for unit in [&trail, &nested] {
                ap.extend_from_slice(&(unit.len() as u16).to_be_bytes());
                ap.extend_from_slice(unit);
            }

            let mut depack = H266Depacketizer::default();
            let mut out = Vec::new();
            let mut extra = CodecExtra::None;
            let res = depack.depacketize(&ap, &mut out, &mut extra);
            assert!(matches!(res, Err(PacketError::ErrH266CorruptedPacket)));
            assert!(out.is_empty());
            Ok(())
        }

        /// a length field larger than
        /// the remaining payload errors — even after valid leading units,
        /// and without leaking partial output.
        #[test]
        fn test_h266_ap_rejects_truncated_tail() -> Result<()> {
            let a = make_nal(0, 4);
            let b = make_nal(1, 4);

            let mut ap = hdr(H266NALU_AGGREGATION_PACKET_TYPE, 0, 1).to_vec();
            for unit in [&a, &b] {
                ap.extend_from_slice(&(unit.len() as u16).to_be_bytes());
                ap.extend_from_slice(unit);
            }
            // third unit claims 0x00ff bytes but provides 1
            ap.extend_from_slice(&[0x00, 0xFF, 0x42]);

            let mut depack = H266Depacketizer::default();
            let mut out = Vec::new();
            let mut extra = CodecExtra::None;
            let res = depack.depacketize(&ap, &mut out, &mut extra);
            assert!(matches!(res, Err(PacketError::ErrShortPacket)));
            assert!(
                out.is_empty(),
                "valid leading units must not leak from a malformed AP"
            );
            Ok(())
        }

        /// a NAL with
        /// F, Z, LayerId and TID all set survives fragmentation and
        /// reassembly bit-exact (packetizer side; the depacketizer rejects
        /// F=1 packets by policy).
        #[test]
        fn test_h266_fu_preserves_all_header_flags() -> Result<()> {
            let mut nal = hdr(1, 1, 1).to_vec();
            nal[0] |= 0b1100_0000; // F + Z
            nal.extend(std::iter::repeat_n(0x5A, 400));

            let mut pck = H266Packetizer::default();
            let packets = pck.packetize(100, &nal)?;
            assert!(packets.len() > 1);

            for p in &packets {
                let h = H266NALUHeader::new(p[0], p[1]);
                assert!(h.f() && h.z(), "F/Z must be copied into FU headers");
                assert_eq!(h.layer_id(), 1);
                assert_eq!(h.tid(), 1);
            }
            assert_eq!(reconstruct_from_fu_packets(&packets), nal);
            Ok(())
        }
    }
}
