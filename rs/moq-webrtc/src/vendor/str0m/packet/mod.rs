#![allow(clippy::type_complexity)]

use std::fmt;
use std::panic::UnwindSafe;

use crate::format::Codec;
use crate::sdp::MediaType;

use crate::rtp::vla::encode_leb_u63;

mod av1;
pub use av1::Av1CodecExtra;
pub use av1::detect_av1_keyframe;
use av1::{Av1Depacketizer, Av1Packetizer};

mod amr_wb;
use amr_wb::{AmrWbDepacketizer, AmrWbPacketizer};

mod g7xx;
use g7xx::{G711Depacketizer, G711Packetizer, G722Packetizer};

mod h264;
pub use h264::H264CodecExtra;
pub use h264::{H264Depacketizer, H264Packetizer};
pub use h264::{detect_h264_keyframe, detect_h264_nalu};

mod h264_profile;
pub(crate) use h264_profile::H264ProfileLevel;

mod h265;
pub use h265::H265CodecExtra;
use h265::H265Depacketizer;
pub use h265::H265Packetizer;
pub use h265::detect_h265_keyframe;

mod h265_profile;
pub use h265_profile::{H265Level, H265Profile, H265ProfileTierLevel, H265Tier};

mod h266;
pub use h266::H266CodecExtra;
use h266::H266Depacketizer;
pub use h266::H266Packetizer;
pub use h266::detect_h266_keyframe;

mod h266_profile;
pub use h266_profile::{H266Level, H266Profile, H266ProfileTierLevel, H266Tier};

mod opus;
pub use opus::{OpusDepacketizer, OpusPacketizer};

mod red;
pub(crate) use red::{MAX_RED_RECOVERY_DEPTH, RedSender, red_same_pt_blocks};
pub use red::{RedBlock, RedDecoder, RedEncoder, RedundantBlock};

mod vp8;
pub use vp8::Vp8CodecExtra;
pub use vp8::Vp8Descriptor;
pub use vp8::Vp8DescriptorError;
pub use vp8::Vp8Patch;
pub use vp8::Vp8PatchBuilder;
pub use vp8::Vp8PatchError;
pub use vp8::detect_vp8_keyframe;
pub use vp8::{Vp8Depacketizer, Vp8Packetizer};

mod vp9;
pub use vp9::Vp9CodecExtra;
pub use vp9::Vp9PacketizerMode;
pub use vp9::detect_vp9_keyframe;
use vp9::{Vp9Depacketizer, Vp9Packetizer};

mod null;
use null::{NullDepacketizer, NullPacketizer};

mod comfort_noise;
use comfort_noise::{ComfortNoiseDepacketizer, ComfortNoisePacketizer};

mod tele;
pub use tele::TelephoneEvent;
pub(crate) use tele::duration_from_units;
use tele::{TelephoneEventDepacketizer, TelephoneEventPacketizer};

mod buffer_rx;
pub(crate) use buffer_rx::{DepacketizingBuffer, RtpMeta};

mod contiguity;
mod contiguity_vp8;
mod contiguity_vp9;
mod error;

mod payload;
pub(crate) use payload::{Payloader, RedSink};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
/// Types of media.
pub enum MediaKind {
    /// Audio media.
    Audio,
    /// Video media.
    Video,
}

#[cfg(feature = "drv")]
crate::drv_identity_copy!(MediaKind);

impl MediaKind {
    /// Tests if this is `MediaKind::Audio`
    pub fn is_audio(&self) -> bool {
        *self == MediaKind::Audio
    }

    /// Tests if this is `MediaKind::Video`
    pub fn is_video(&self) -> bool {
        *self == MediaKind::Video
    }
}

/// Packetizes some bytes for use as RTP packet.
///
/// ## Unversioned API surface
///
/// This trait is not currently versioned according to semver rules.
/// Breaking changes may be made in minor or patch releases.
pub trait Packetizer: fmt::Debug {
    /// Chunk the data up into RTP packets.
    fn packetize(&mut self, mtu: usize, b: &[u8]) -> Result<Vec<Vec<u8>>, PacketError>;

    /// Tell if this is the last packet of a frame.
    fn is_marker(&mut self, data: &[u8], previous: Option<&[u8]>, last: bool) -> bool;

    /// Whether a start-of-talkspurt should set the RTP marker bit for this codec. Audio codecs
    /// that mark talkspurt boundaries return `true`; video and comfort noise return `false`.
    fn marks_talkspurt(&self) -> bool {
        false
    }

    /// Default nackability of packets this packetizer produces. Video is nackable (answered via
    /// RTX); audio is not, so audio packetizers override this to `false`.
    fn nackable(&self) -> bool {
        true
    }
}

/// Codec specific information
///
/// Contains additional codec specific information which are deemed useful for
/// managing and repackaging the frame
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum CodecExtra {
    /// No extra information available
    None,
    /// Codec extra parameters for VP8.
    Vp8(Vp8CodecExtra),
    /// Codec extra parameters for VP9.
    Vp9(Vp9CodecExtra),
    /// Codec extra parameters for H264.
    H264(H264CodecExtra),
    /// Codec extra parameters for AV1,
    Av1(Av1CodecExtra),
    /// Codec extra parameters for H265.
    H265(H265CodecExtra),
    /// Codec extra parameters for H266 (VVC). Prototype.
    H266(H266CodecExtra),
    /// The telephone-event (RFC 4733) value in this media sample.
    Tele(TelephoneEvent),
}

/// Depacketizes an RTP payload.
///
/// Removes any RTP specific data from the payload.
///
/// ## Unversioned API surface
///
/// This trait is not currently versioned according to semver rules.
/// Breaking changes may be made in minor or patch releases.
pub trait Depacketizer: fmt::Debug {
    /// Provide a size hint for the `out: &mut Vec<u8>` parameter in [`Depacketizer::depacketize`].
    /// The [`packets_size`] parameter is the sum of the size of all packets that will be processed in
    /// subsequent calls to [`Depacketizer::depacketize`].
    /// This is used to allocate the vector with upfront capacity.
    /// Return [`None`] if it cannot be determined.
    fn out_size_hint(&self, packets_size: usize) -> Option<usize>;

    /// Unpack the RTP packet into a provided `Vec<u8>`.
    fn depacketize(
        &mut self,
        packet: &[u8],
        out: &mut Vec<u8>,
        codec_extra: &mut CodecExtra,
    ) -> Result<(), PacketError>;

    /// Checks if the packet is at the beginning of a partition.
    ///
    /// Returns false if the result could not be determined.
    fn is_partition_head(&self, packet: &[u8]) -> bool;

    /// Checks if the packet is at the end of a partition.
    ///
    /// Returns false if the result could not be determined.
    fn is_partition_tail(&self, marker: bool, packet: &[u8]) -> bool;
}

pub use error::PacketError;

/// Helper to replace Bytes. Provides get_u8 and get_u16 over some buffer of bytes.
pub(crate) trait BitRead {
    fn remaining_bits(&self) -> usize;
    fn remaining_bytes(&self) -> usize;
    fn byte_offset(&self) -> usize;
    fn get_u8(&mut self) -> Option<u8>;
    fn get_u16(&mut self) -> Option<u16>;
    /// Reads `count` bits, at most 8, right-aligned in the result.
    fn get_bits(&mut self, count: usize) -> Option<u8>;
    fn get_remaining(&mut self) -> &[u8];
    fn get_bytes(&mut self, count: usize) -> Option<&[u8]>;
    fn get_leb128(&mut self) -> Option<usize>;
    fn skip_bytes(&mut self, count: usize);
}

impl BitRead for (&[u8], usize) {
    #[inline(always)]
    fn remaining_bits(&self) -> usize {
        (self.0.len() * 8).saturating_sub(self.1)
    }

    #[inline(always)]
    fn remaining_bytes(&self) -> usize {
        self.remaining_bits() / 8
    }

    #[inline(always)]
    fn byte_offset(&self) -> usize {
        self.1 / 8
    }

    #[inline(always)]
    fn get_u8(&mut self) -> Option<u8> {
        if self.remaining_bits() < 8 {
            return None;
        }

        let offs = self.1 / 8;
        let shift = (self.1 % 8) as u32;
        self.1 += 8;

        let mut n = self.0[offs];

        if shift > 0 {
            n <<= shift;
            n |= self.0[offs + 1] >> (8 - shift)
        }

        Some(n)
    }

    fn get_u16(&mut self) -> Option<u16> {
        if self.remaining_bits() < 16 {
            return None;
        }
        Some(u16::from_be_bytes([self.get_u8()?, self.get_u8()?]))
    }

    fn get_bits(&mut self, count: usize) -> Option<u8> {
        if count > 8 || self.remaining_bits() < count {
            return None;
        }
        if count == 0 {
            return Some(0);
        }

        let offs = self.1 / 8;
        let next = self.0.get(offs + 1).copied().unwrap_or(0);
        let window = u16::from_be_bytes([self.0[offs], next]) << (self.1 % 8);
        self.1 += count;

        Some((window >> (16 - count)) as u8)
    }

    fn get_remaining(&mut self) -> &[u8] {
        let offset = self.1 / 8;
        self.1 = self.0.len() * 8;
        &self.0[offset..]
    }

    fn get_bytes(&mut self, count: usize) -> Option<&[u8]> {
        let count_bits = count.saturating_mul(8);
        if self.remaining_bits() < count_bits {
            return None;
        }
        let offset = self.1 / 8;
        self.1 += count_bits;
        Some(&self.0[offset..offset + count])
    }

    fn get_leb128(&mut self) -> Option<usize> {
        let mut temp_value: usize = 0;

        for i in (0..64).step_by(7) {
            if self.remaining_bits() < 8 {
                return None;
            }
            let byte = self.get_u8().unwrap();
            temp_value |= ((byte & 0x7f) as usize) << i;

            if byte < 0x80 {
                return Some(temp_value);
            }
        }

        None
    }

    fn skip_bytes(&mut self, count: usize) {
        let bits = count.saturating_mul(8);
        if self.remaining_bits() < bits {
            return;
        }

        self.1 += bits;
    }
}

#[derive(Debug)]
pub(crate) enum CodecPacketizer {
    #[allow(unused)]
    G711(G711Packetizer),
    #[allow(unused)]
    G722(G722Packetizer),
    H264(H264Packetizer),
    H265(H265Packetizer),
    H266(H266Packetizer),
    Opus(OpusPacketizer),
    ComfortNoise(ComfortNoisePacketizer),
    Tele(TelephoneEventPacketizer),
    Vp8(Vp8Packetizer),
    Vp9(Vp9Packetizer),
    Av1(Av1Packetizer),
    AmrWb(AmrWbPacketizer),
    Null(NullPacketizer),
    #[allow(unused)]
    Boxed(Box<dyn Packetizer + Send + Sync + UnwindSafe>),
}

#[derive(Debug)]
pub(crate) enum CodecDepacketizer {
    G711(G711Depacketizer),
    H264(H264Depacketizer),
    H265(H265Depacketizer),
    H266(H266Depacketizer),
    Opus(OpusDepacketizer),
    ComfortNoise(ComfortNoiseDepacketizer),
    Tele(TelephoneEventDepacketizer),
    Vp8(Vp8Depacketizer),
    Vp9(Vp9Depacketizer),
    Av1(Av1Depacketizer),
    AmrWb(AmrWbDepacketizer),
    Null(NullDepacketizer),
    #[allow(unused)]
    Boxed(Box<dyn Depacketizer + Send + Sync + UnwindSafe>),
}

impl CodecPacketizer {
    pub(crate) fn new(codec: Codec, vp9_mode: Vp9PacketizerMode) -> Self {
        match codec {
            Codec::Opus => CodecPacketizer::Opus(OpusPacketizer),
            Codec::PCMU => CodecPacketizer::G711(G711Packetizer::default()),
            Codec::PCMA => CodecPacketizer::G711(G711Packetizer::default()),
            Codec::G722 => CodecPacketizer::G722(G722Packetizer::default()),
            Codec::CN => CodecPacketizer::ComfortNoise(ComfortNoisePacketizer),
            Codec::AmrWb => CodecPacketizer::AmrWb(AmrWbPacketizer::default()),
            Codec::H264 => CodecPacketizer::H264(H264Packetizer::default()),
            Codec::H265 => CodecPacketizer::H265(H265Packetizer::default()),
            Codec::H266 => CodecPacketizer::H266(H266Packetizer::default()),
            Codec::Vp8 => CodecPacketizer::Vp8(Vp8Packetizer::default()),
            Codec::Vp9 => CodecPacketizer::Vp9(Vp9Packetizer::with_mode(vp9_mode)),
            Codec::Av1 => CodecPacketizer::Av1(Av1Packetizer::default()),
            Codec::Null => CodecPacketizer::Null(NullPacketizer),
            Codec::Tele => CodecPacketizer::Tele(TelephoneEventPacketizer),
            Codec::Rtx => panic!("Cant instantiate packetizer for RTX codec"),
            Codec::Red => panic!("Cant instantiate packetizer for RED codec"),
            Codec::Unknown => panic!("Cant instantiate packetizer for unknown codec"),
        }
    }

    /// Apply codec-specific configuration derived from the negotiated [`CodecSpec`]. Keeps this
    /// knowledge inside the codec-aware packetizer so the generic [`Payloader`] never has to name
    /// concrete packetizer variants.
    pub(crate) fn configure_for(&mut self, spec: &crate::format::CodecSpec) {
        // Enable DONL for H.265 when sprop-max-don-diff > 0 (RFC 7798 §7.1).
        if let CodecPacketizer::H265(h265) = self {
            if spec.format.sprop_max_don_diff.unwrap_or(0) > 0 {
                h265.with_donl(true);
            }
        }
        // Enable DONL for H.266 when sprop-max-don-diff > 0 (RFC 9328 §7.2).
        if let CodecPacketizer::H266(h266) = self {
            if spec.format.sprop_max_don_diff.unwrap_or(0) > 0 {
                h266.with_donl(true);
            }
        }
        if let CodecPacketizer::AmrWb(amr) = self {
            amr.octet_align = spec.format.octet_align.unwrap_or(false);
        }
    }
}

impl From<Codec> for CodecPacketizer {
    fn from(c: Codec) -> Self {
        CodecPacketizer::new(c, Vp9PacketizerMode::default())
    }
}

impl From<Codec> for CodecDepacketizer {
    fn from(c: Codec) -> Self {
        match c {
            Codec::Opus => CodecDepacketizer::Opus(OpusDepacketizer),
            Codec::PCMU => CodecDepacketizer::G711(G711Depacketizer),
            Codec::PCMA => CodecDepacketizer::G711(G711Depacketizer),
            Codec::G722 => CodecDepacketizer::G711(G711Depacketizer),
            Codec::CN => CodecDepacketizer::ComfortNoise(ComfortNoiseDepacketizer),
            Codec::AmrWb => CodecDepacketizer::AmrWb(AmrWbDepacketizer::default()),
            Codec::H264 => CodecDepacketizer::H264(H264Depacketizer::default()),
            Codec::H265 => CodecDepacketizer::H265(H265Depacketizer::default()),
            Codec::H266 => CodecDepacketizer::H266(H266Depacketizer::default()),
            Codec::Vp8 => CodecDepacketizer::Vp8(Vp8Depacketizer::default()),
            Codec::Vp9 => CodecDepacketizer::Vp9(Vp9Depacketizer::default()),
            Codec::Av1 => CodecDepacketizer::Av1(Av1Depacketizer::default()),
            Codec::Null => CodecDepacketizer::Null(NullDepacketizer),
            Codec::Tele => CodecDepacketizer::Tele(TelephoneEventDepacketizer::default()),
            Codec::Rtx => panic!("Cant instantiate depacketizer for RTX codec"),
            Codec::Red => panic!("Cant instantiate depacketizer for RED codec"),
            Codec::Unknown => panic!("Cant instantiate depacketizer for unknown codec"),
        }
    }
}

impl Packetizer for CodecPacketizer {
    fn packetize(&mut self, mtu: usize, b: &[u8]) -> Result<Vec<Vec<u8>>, PacketError> {
        use CodecPacketizer::*;
        match self {
            G711(v) => v.packetize(mtu, b),
            G722(v) => v.packetize(mtu, b),
            H264(v) => v.packetize(mtu, b),
            H265(v) => v.packetize(mtu, b),
            H266(v) => v.packetize(mtu, b),
            Opus(v) => v.packetize(mtu, b),
            ComfortNoise(v) => v.packetize(mtu, b),
            Tele(v) => v.packetize(mtu, b),
            Vp8(v) => v.packetize(mtu, b),
            Vp9(v) => v.packetize(mtu, b),
            Av1(v) => v.packetize(mtu, b),
            AmrWb(v) => v.packetize(mtu, b),
            Null(v) => v.packetize(mtu, b),
            Boxed(v) => v.packetize(mtu, b),
        }
    }

    fn is_marker(&mut self, data: &[u8], previous: Option<&[u8]>, last: bool) -> bool {
        match self {
            CodecPacketizer::G711(v) => v.is_marker(data, previous, last),
            CodecPacketizer::G722(v) => v.is_marker(data, previous, last),
            CodecPacketizer::Opus(v) => v.is_marker(data, previous, last),
            CodecPacketizer::ComfortNoise(v) => v.is_marker(data, previous, last),
            CodecPacketizer::Tele(v) => v.is_marker(data, previous, last),
            CodecPacketizer::H264(v) => v.is_marker(data, previous, last),
            CodecPacketizer::H265(v) => v.is_marker(data, previous, last),
            CodecPacketizer::H266(v) => v.is_marker(data, previous, last),
            CodecPacketizer::Vp8(v) => v.is_marker(data, previous, last),
            CodecPacketizer::Vp9(v) => v.is_marker(data, previous, last),
            CodecPacketizer::Av1(v) => v.is_marker(data, previous, last),
            CodecPacketizer::AmrWb(v) => v.is_marker(data, previous, last),
            CodecPacketizer::Null(v) => v.is_marker(data, previous, last),
            CodecPacketizer::Boxed(v) => v.is_marker(data, previous, last),
        }
    }

    fn marks_talkspurt(&self) -> bool {
        use CodecPacketizer::*;
        match self {
            G711(v) => v.marks_talkspurt(),
            G722(v) => v.marks_talkspurt(),
            H264(v) => v.marks_talkspurt(),
            H265(v) => v.marks_talkspurt(),
            H266(v) => v.marks_talkspurt(),
            Opus(v) => v.marks_talkspurt(),
            ComfortNoise(v) => v.marks_talkspurt(),
            Tele(v) => v.marks_talkspurt(),
            Vp8(v) => v.marks_talkspurt(),
            Vp9(v) => v.marks_talkspurt(),
            Av1(v) => v.marks_talkspurt(),
            AmrWb(v) => v.marks_talkspurt(),
            Null(v) => v.marks_talkspurt(),
            Boxed(v) => v.marks_talkspurt(),
        }
    }

    fn nackable(&self) -> bool {
        use CodecPacketizer::*;
        match self {
            G711(v) => v.nackable(),
            G722(v) => v.nackable(),
            H264(v) => v.nackable(),
            H265(v) => v.nackable(),
            H266(v) => v.nackable(),
            Opus(v) => v.nackable(),
            ComfortNoise(v) => v.nackable(),
            Tele(v) => v.nackable(),
            Vp8(v) => v.nackable(),
            Vp9(v) => v.nackable(),
            Av1(v) => v.nackable(),
            AmrWb(v) => v.nackable(),
            Null(v) => v.nackable(),
            Boxed(v) => v.nackable(),
        }
    }
}

impl Depacketizer for CodecDepacketizer {
    fn out_size_hint(&self, packets_size: usize) -> Option<usize> {
        use CodecDepacketizer::*;
        match self {
            H264(v) => v.out_size_hint(packets_size),
            H265(v) => v.out_size_hint(packets_size),
            H266(v) => v.out_size_hint(packets_size),
            Opus(v) => v.out_size_hint(packets_size),
            ComfortNoise(v) => v.out_size_hint(packets_size),
            Tele(v) => v.out_size_hint(packets_size),
            G711(v) => v.out_size_hint(packets_size),
            Vp8(v) => v.out_size_hint(packets_size),
            Vp9(v) => v.out_size_hint(packets_size),
            Av1(v) => v.out_size_hint(packets_size),
            AmrWb(v) => v.out_size_hint(packets_size),
            Null(v) => v.out_size_hint(packets_size),
            Boxed(v) => v.out_size_hint(packets_size),
        }
    }

    fn depacketize(
        &mut self,
        packet: &[u8],
        out: &mut Vec<u8>,
        extra: &mut CodecExtra,
    ) -> Result<(), PacketError> {
        use CodecDepacketizer::*;
        match self {
            H264(v) => v.depacketize(packet, out, extra),
            H265(v) => v.depacketize(packet, out, extra),
            H266(v) => v.depacketize(packet, out, extra),
            Opus(v) => v.depacketize(packet, out, extra),
            ComfortNoise(v) => v.depacketize(packet, out, extra),
            Tele(v) => v.depacketize(packet, out, extra),
            G711(v) => v.depacketize(packet, out, extra),
            Vp8(v) => v.depacketize(packet, out, extra),
            Vp9(v) => v.depacketize(packet, out, extra),
            Av1(v) => v.depacketize(packet, out, extra),
            AmrWb(v) => v.depacketize(packet, out, extra),
            Null(v) => v.depacketize(packet, out, extra),
            Boxed(v) => v.depacketize(packet, out, extra),
        }
    }

    fn is_partition_head(&self, packet: &[u8]) -> bool {
        use CodecDepacketizer::*;
        match self {
            H264(v) => v.is_partition_head(packet),
            H265(v) => v.is_partition_head(packet),
            H266(v) => v.is_partition_head(packet),
            Opus(v) => v.is_partition_head(packet),
            ComfortNoise(v) => v.is_partition_head(packet),
            Tele(v) => v.is_partition_head(packet),
            G711(v) => v.is_partition_head(packet),
            Vp8(v) => v.is_partition_head(packet),
            Vp9(v) => v.is_partition_head(packet),
            Av1(v) => v.is_partition_head(packet),
            AmrWb(v) => v.is_partition_head(packet),
            Null(v) => v.is_partition_head(packet),
            Boxed(v) => v.is_partition_head(packet),
        }
    }

    fn is_partition_tail(&self, marker: bool, packet: &[u8]) -> bool {
        use CodecDepacketizer::*;
        match self {
            H264(v) => v.is_partition_tail(marker, packet),
            H265(v) => v.is_partition_tail(marker, packet),
            H266(v) => v.is_partition_tail(marker, packet),
            Opus(v) => v.is_partition_tail(marker, packet),
            ComfortNoise(v) => v.is_partition_tail(marker, packet),
            Tele(v) => v.is_partition_tail(marker, packet),
            G711(v) => v.is_partition_tail(marker, packet),
            Vp8(v) => v.is_partition_tail(marker, packet),
            Vp9(v) => v.is_partition_tail(marker, packet),
            Av1(v) => v.is_partition_tail(marker, packet),
            AmrWb(v) => v.is_partition_tail(marker, packet),
            Null(v) => v.is_partition_tail(marker, packet),
            Boxed(v) => v.is_partition_tail(marker, packet),
        }
    }
}

impl From<MediaType> for MediaKind {
    fn from(v: MediaType) -> Self {
        match v {
            MediaType::Audio => MediaKind::Audio,
            MediaType::Video => MediaKind::Video,
            _ => panic!("Not MediaType::Audio or Video"),
        }
    }
}

impl fmt::Display for MediaKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MediaKind::Audio => write!(f, "audio"),
            MediaKind::Video => write!(f, "video"),
        }
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn bit_read_get_bits() {
        let mut reader: (&[u8], usize) = (&[0b1010_1100, 0b0101_0000], 4);
        assert_eq!(reader.get_bits(6), Some(0b110001));
        assert_eq!(reader.get_bits(3), Some(0b010));
        assert_eq!(reader.get_bits(9), None);
        assert_eq!(reader.get_bits(4), None);
        assert_eq!(reader.get_bits(3), Some(0));
        assert_eq!(reader.get_bits(0), Some(0));
    }

    #[test]
    fn comfort_noise_packetizer_preserves_payload_and_clears_marker() {
        let payload = [42, 128, 64];
        let mut packetizer = CodecPacketizer::from(Codec::CN);

        assert_eq!(
            packetizer.packetize(1200, &payload).unwrap(),
            vec![payload.to_vec()]
        );
        assert!(!packetizer.is_marker(&payload, None, true));

        let mut depacketizer = CodecDepacketizer::from(Codec::CN);
        let mut output = Vec::new();
        depacketizer
            .depacketize(&payload, &mut output, &mut CodecExtra::None)
            .unwrap();
        assert_eq!(output, payload);
        assert!(depacketizer.is_partition_head(&payload));
        assert!(depacketizer.is_partition_tail(false, &payload));
    }

    #[test]
    fn telephone_event_codec_uses_telephone_event_packetizers() {
        let payload = [5, 0x80, 0, 160];
        let mut packetizer = CodecPacketizer::from(Codec::Tele);
        assert_eq!(
            packetizer.packetize(1200, &payload).unwrap(),
            vec![payload.to_vec()]
        );
        assert!(!packetizer.is_marker(&payload, None, true));
        assert!(packetizer.marks_talkspurt());
        assert!(!packetizer.nackable());

        let mut depacketizer = CodecDepacketizer::from(Codec::Tele);
        let mut output = Vec::new();
        let mut extra = CodecExtra::None;
        depacketizer
            .depacketize(&payload, &mut output, &mut extra)
            .unwrap();
        assert_eq!(output, payload);
        let report = TelephoneEvent::parse(&payload, crate::rtp_::Frequency::EIGHT_KHZ).unwrap();
        assert_eq!(extra, CodecExtra::Tele(report));
    }
}
