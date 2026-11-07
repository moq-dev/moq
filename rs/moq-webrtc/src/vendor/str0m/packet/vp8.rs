use std::error::Error;
use std::fmt;

use crate::rtp_::{extend_u7, extend_u8, extend_u15};

use super::{BitRead, CodecExtra, Depacketizer, PacketError, Packetizer};

pub const VP8_HEADER_SIZE: usize = 1;

const PICTURE_ID_SHORT_MAX: u16 = 0x7f; // 7-bit PictureID, excluding the M bit
const PICTURE_ID_MAX: u16 = 0x7fff; // 15-bit PictureID, excluding the M bit
const PICTURE_ID_SHORT_MASK: u8 = PICTURE_ID_SHORT_MAX as u8;
const KEY_IDX_MASK: u8 = 0x1f; // 5-bit KEYIDX mask within the TID/Y/KEYIDX octet
const VP8_PATCH_BYTES: usize = 4; // two PictureID bytes, TL0PICIDX and KEYIDX

/// Validated byte replacements for a VP8 payload descriptor.
///
/// Build this from [`Vp8Descriptor::patch`] when the caller already parsed the
/// payload descriptor before fanout. The patch is fixed-size and never changes
/// payload length.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Vp8Patch {
    len: u8,
    offsets: [u8; VP8_PATCH_BYTES],
    bytes: [u8; VP8_PATCH_BYTES],
}

impl Vp8Patch {
    fn new() -> Self {
        Self {
            len: 0,
            offsets: [0; VP8_PATCH_BYTES],
            bytes: [0; VP8_PATCH_BYTES],
        }
    }

    fn push(&mut self, offset: usize, byte: u8) {
        let index = usize::from(self.len);
        debug_assert!(index < self.offsets.len());
        debug_assert!(offset <= usize::from(u8::MAX));

        self.offsets[index] = offset as u8;
        self.bytes[index] = byte;
        self.len += 1;
    }

    /// Copy `payload` into `dst` while applying VP8 descriptor byte replacements.
    ///
    /// # Panics
    ///
    /// Panics if `dst` is not the same length as `payload`.
    pub(crate) fn copy_to(&self, payload: &[u8], dst: &mut [u8]) {
        assert_eq!(payload.len(), dst.len());

        dst.copy_from_slice(payload);

        for i in 0..usize::from(self.len) {
            dst[usize::from(self.offsets[i])] = self.bytes[i];
        }
    }
}

/// Errors returned when a VP8 payload descriptor cannot be parsed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum Vp8DescriptorError {
    /// The payload is too short to contain a valid VP8 payload descriptor.
    ShortPayload,
    /// The payload descriptor violates VP8 payload descriptor rules.
    MalformedDescriptor,
}

impl fmt::Display for Vp8DescriptorError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ShortPayload => write!(f, "VP8 payload is too short"),
            Self::MalformedDescriptor => write!(f, "VP8 payload descriptor is malformed"),
        }
    }
}

impl Error for Vp8DescriptorError {}

/// Errors returned when a VP8 payload descriptor patch cannot be built.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum Vp8PatchError {
    /// The payload descriptor does not contain a PictureID field.
    PictureIdMissing,
    /// The requested PictureID does not fit the existing descriptor width.
    PictureIdTooLarge,
    /// The payload descriptor does not contain a TL0PICIDX field.
    Tl0PicIdxMissing,
    /// The payload descriptor does not contain a KEYIDX field.
    KeyIdxMissing,
    /// The requested KEYIDX does not fit the five-bit KEYIDX field.
    KeyIdxTooLarge,
}

impl fmt::Display for Vp8PatchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::PictureIdMissing => write!(f, "VP8 payload descriptor has no PictureID"),
            Self::PictureIdTooLarge => write!(f, "VP8 PictureID does not fit descriptor width"),
            Self::Tl0PicIdxMissing => write!(f, "VP8 payload descriptor has no TL0PICIDX"),
            Self::KeyIdxMissing => write!(f, "VP8 payload descriptor has no KEYIDX"),
            Self::KeyIdxTooLarge => write!(f, "VP8 KEYIDX must fit in 5 bits"),
        }
    }
}

impl Error for Vp8PatchError {}

/// Builder for fixed-size VP8 payload descriptor patches.
#[derive(Clone, Copy, Debug)]
pub struct Vp8PatchBuilder {
    descriptor: Vp8Descriptor,
    picture_id: Option<u16>,
    tl0_pic_idx: Option<u8>,
    key_idx: Option<u8>,
}

impl Vp8PatchBuilder {
    /// Rewrite the VP8 PictureID field.
    ///
    /// The parsed descriptor decides whether the existing descriptor uses the
    /// 7-bit or 15-bit PictureID representation. Rewriting preserves that
    /// representation.
    ///
    /// [`Vp8PatchBuilder::build`] returns [`Vp8PatchError::PictureIdTooLarge`]
    /// if the value does not fit the existing representation.
    pub fn picture_id(mut self, picture_id: u16) -> Self {
        self.picture_id = Some(picture_id);
        self
    }

    /// Rewrite the VP8 TL0PICIDX field.
    ///
    /// The rewrite is valid only when the payload descriptor already contains an
    /// TL0PICIDX field.
    pub fn tl0_pic_idx(mut self, tl0_pic_idx: u8) -> Self {
        self.tl0_pic_idx = Some(tl0_pic_idx);
        self
    }

    /// Rewrite the VP8 KEYIDX field.
    ///
    /// Only the lower five KEYIDX bits are replaced. The TID and Y bits in the
    /// same descriptor octet are preserved.
    ///
    /// [`Vp8PatchBuilder::build`] returns [`Vp8PatchError::KeyIdxTooLarge`] if
    /// the value does not fit the five-bit KEYIDX field.
    pub fn key_idx(mut self, key_idx: u8) -> Self {
        self.key_idx = Some(key_idx);
        self
    }

    /// Build a validated fixed-size VP8 descriptor patch.
    ///
    /// # Errors
    ///
    /// Returns [`Vp8PatchError::PictureIdMissing`],
    /// [`Vp8PatchError::Tl0PicIdxMissing`] or [`Vp8PatchError::KeyIdxMissing`]
    /// when the requested rewrite targets a descriptor field that is absent.
    /// Returns [`Vp8PatchError::PictureIdTooLarge`] or
    /// [`Vp8PatchError::KeyIdxTooLarge`] when a requested rewrite value does
    /// not fit the field representation.
    pub fn build(self) -> Result<Vp8Patch, Vp8PatchError> {
        let mut patch = Vp8Patch::new();

        if let Some(picture_id) = self.picture_id {
            let Some(parsed) = self.descriptor.picture_id else {
                return Err(Vp8PatchError::PictureIdMissing);
            };
            if picture_id > PICTURE_ID_MAX {
                return Err(Vp8PatchError::PictureIdTooLarge);
            }
            if !parsed.is_15_bit && picture_id > PICTURE_ID_SHORT_MAX {
                return Err(Vp8PatchError::PictureIdTooLarge);
            }
            if parsed.is_15_bit {
                patch.push(
                    parsed.offset,
                    0x80 | ((picture_id >> 8) as u8 & PICTURE_ID_SHORT_MASK),
                );
                patch.push(parsed.offset + 1, picture_id as u8);
            } else {
                patch.push(parsed.offset, picture_id as u8);
            }
        }

        if let Some(tl0_pic_idx) = self.tl0_pic_idx {
            let Some(parsed) = self.descriptor.tl0_pic_idx else {
                return Err(Vp8PatchError::Tl0PicIdxMissing);
            };
            patch.push(parsed.offset, tl0_pic_idx);
        }

        if let Some(key_idx) = self.key_idx {
            let Some(parsed) = self.descriptor.key_idx else {
                return Err(Vp8PatchError::KeyIdxMissing);
            };
            if key_idx > KEY_IDX_MASK {
                return Err(Vp8PatchError::KeyIdxTooLarge);
            }
            patch.push(parsed.offset, (parsed.value & !KEY_IDX_MASK) | key_idx);
        }

        Ok(patch)
    }
}

/// Parsed VP8 RTP payload descriptor.
///
/// The descriptor stores VP8 fields plus private offsets needed to
/// build a validated [`Vp8Patch`]. It is valid only for the payload bytes parsed
/// by [`Vp8Descriptor::parse`] or a byte-identical copy of those bytes.
#[derive(Clone, Copy, Debug)]
pub struct Vp8Descriptor {
    payload_offset: usize,
    picture_id: Option<Vp8PictureId>,
    tl0_pic_idx: Option<Vp8Field>,
    key_idx: Option<Vp8Field>,
}

#[derive(Clone, Copy, Debug)]
struct Vp8Field {
    value: u8,
    offset: usize,
}

#[derive(Clone, Copy, Debug)]
struct Vp8PictureId {
    value: u16,
    offset: usize,
    is_15_bit: bool,
}

impl Vp8Descriptor {
    /// Parse a VP8 RTP payload descriptor
    ///
    /// The input must be the RTP payload only. It must not include the RTP
    /// header.
    ///
    /// # Errors
    ///
    /// Returns [`Vp8DescriptorError::ShortPayload`] when the payload is truncated.
    /// Returns [`Vp8DescriptorError::MalformedDescriptor`] when the descriptor
    /// has unsupported field combinations.
    pub fn parse(payload: &[u8]) -> Result<Self, Vp8DescriptorError> {
        let Some(required) = payload.first() else {
            return Err(Vp8DescriptorError::ShortPayload);
        };

        let mut descriptor = Self {
            payload_offset: 1,
            picture_id: None,
            tl0_pic_idx: None,
            key_idx: None,
        };

        if required & 0x80 != 0 {
            let extension = read_vp8_descriptor_byte(payload, &mut descriptor.payload_offset)?;
            let has_picture_id = extension & 0x80 != 0;
            let has_tl0_pic_idx = extension & 0x40 != 0;
            let has_tid = extension & 0x20 != 0;
            let has_key_idx = extension & 0x10 != 0;

            if has_tl0_pic_idx && !has_tid {
                return Err(Vp8DescriptorError::MalformedDescriptor);
            }

            if has_picture_id {
                let picture_id_offset = descriptor.payload_offset;
                let picture_id = read_vp8_descriptor_byte(payload, &mut descriptor.payload_offset)?;
                let is_15_bit = picture_id & 0x80 != 0;
                let value = if is_15_bit {
                    let picture_id_low =
                        read_vp8_descriptor_byte(payload, &mut descriptor.payload_offset)?;
                    (u16::from(picture_id & PICTURE_ID_SHORT_MASK) << 8) | u16::from(picture_id_low)
                } else {
                    u16::from(picture_id & PICTURE_ID_SHORT_MASK)
                };
                descriptor.picture_id = Some(Vp8PictureId {
                    value,
                    offset: picture_id_offset,
                    is_15_bit,
                });
            }

            if has_tl0_pic_idx {
                let offset = descriptor.payload_offset;
                let value = read_vp8_descriptor_byte(payload, &mut descriptor.payload_offset)?;
                descriptor.tl0_pic_idx = Some(Vp8Field { value, offset });
            }

            if has_tid || has_key_idx {
                let offset = descriptor.payload_offset;
                let value = read_vp8_descriptor_byte(payload, &mut descriptor.payload_offset)?;
                if has_key_idx {
                    descriptor.key_idx = Some(Vp8Field { value, offset });
                }
            }
        }

        if descriptor.payload_offset >= payload.len() {
            return Err(Vp8DescriptorError::ShortPayload);
        }

        Ok(descriptor)
    }

    /// Returns the VP8 PictureID if present
    pub const fn picture_id(&self) -> Option<u16> {
        match self.picture_id {
            Some(picture_id) => Some(picture_id.value),
            None => None,
        }
    }

    /// Returns the VP8 TL0PICIDX if present
    pub const fn tl0_pic_idx(&self) -> Option<u8> {
        match self.tl0_pic_idx {
            Some(tl0_pic_idx) => Some(tl0_pic_idx.value),
            None => None,
        }
    }

    /// returns the VP8 KEYIDX if present
    pub const fn key_idx(&self) -> Option<u8> {
        match self.key_idx {
            Some(key_idx) => Some(key_idx.value & KEY_IDX_MASK),
            None => None,
        }
    }

    /// Returns whether this descriptor starts a VP8 keyframe.
    ///
    /// `payload` must be the same payload that produced this descriptor or a
    /// byte-identical copy. A mismatched or shorter payload returns `false`.
    pub fn starts_keyframe(&self, payload: &[u8]) -> bool {
        let Some(required) = payload.first() else {
            return false;
        };

        if required & 0x10 == 0 || required & 0x07 != 0 {
            return false;
        }

        payload
            .get(self.payload_offset)
            .is_some_and(|header| header & 0x01 == 0)
    }

    /// Start building a validated fixed-size VP8 descriptor patch.
    pub fn patch(&self) -> Vp8PatchBuilder {
        Vp8PatchBuilder {
            descriptor: *self,
            picture_id: None,
            tl0_pic_idx: None,
            key_idx: None,
        }
    }
}

fn read_vp8_descriptor_byte(payload: &[u8], offset: &mut usize) -> Result<u8, Vp8DescriptorError> {
    let byte = payload
        .get(*offset)
        .ok_or(Vp8DescriptorError::ShortPayload)?;
    *offset += 1;
    Ok(*byte)
}

/// Vp8 information describing the depacketized / packetized data
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Vp8CodecExtra {
    /// True if the frame can be discarded safely, without causing decoding problems
    /// No other frames are encoded depending on this frame (non-reference frame)
    pub discardable: bool,
    /// True if this frame and subsequent ones on this layer depend only on tl0_pic_idx
    pub sync: bool,
    /// Index of the vp8 temporal layer.
    pub layer_index: u8,
    /// Extended picture id, if present
    pub picture_id: Option<u64>,
    /// Extended picture id of layer 0 frames, if present
    pub tl0_picture_id: Option<u64>,
    /// Flag which indicates that within [`MediaData`], there is an individual frame
    /// containing complete and independent visual information. This frame serves
    /// as a reference point for other frames in the video sequence.
    ///
    /// [`MediaData`]: crate::media::MediaData
    pub is_keyframe: bool,
}

/// Detect whether a VP8 RTP payload contains a keyframe.
///
/// Parses the VP8 RTP payload descriptor (RFC 7741) to skip past the
/// variable-length header, then checks the P bit in the VP8 payload header.
/// P=0 means keyframe, P=1 means interframe.
///
/// Returns `true` only for the first packet of a keyframe (S=1, PID=0).
pub fn detect_vp8_keyframe(payload: &[u8]) -> bool {
    if payload.is_empty() {
        return false;
    }
    let b0 = payload[0];
    let s = (b0 & 0x10) >> 4; // Start of VP8 partition
    let pid = b0 & 0x07; // Partition index
    // Only the first packet of a frame (S=1, PID=0) contains the payload header
    if s != 1 || pid != 0 {
        return false;
    }
    let x = (b0 & 0x80) >> 7; // Extension bit
    let mut idx = 1;
    if x == 1 {
        if idx >= payload.len() {
            return false;
        }
        let ext = payload[idx];
        idx += 1;
        let i = (ext & 0x80) >> 7; // PictureID present
        let l = (ext & 0x40) >> 6; // TL0PICIDX present
        let t = (ext & 0x20) >> 5; // TID present
        let k = (ext & 0x10) >> 4; // KEYIDX present
        if i == 1 {
            if idx >= payload.len() {
                return false;
            }
            if payload[idx] & 0x80 != 0 {
                idx += 2; // 16-bit PictureID
            } else {
                idx += 1; // 7-bit PictureID
            }
        }
        if l == 1 {
            idx += 1; // tl0picidx
        }
        if t == 1 || k == 1 {
            idx += 1; // TID/KEYIDX
        }
    }
    if idx >= payload.len() {
        return false;
    }
    // VP8 Payload Header: P bit is bit 0 of the first byte
    // P=0 → keyframe, P=1 → interframe
    payload[idx] & 0x01 == 0
}

/// Packetizes VP8 RTP packets.
///
/// ## Unversioned API surface
///
/// This struct is not currently versioned according to semver rules.
/// Breaking changes may be made in minor or patch releases.
#[derive(Default, Debug, Copy, Clone)]
pub struct Vp8Packetizer {
    enable_picture_id: bool,
    picture_id: u16,
}

impl Packetizer for Vp8Packetizer {
    /// Payload fragments a VP8 packet across one or more byte arrays
    fn packetize(&mut self, mtu: usize, payload: &[u8]) -> Result<Vec<Vec<u8>>, PacketError> {
        if payload.is_empty() || mtu == 0 {
            return Ok(vec![]);
        }

        /*
         * https://tools.ietf.org/html/rfc7741#section-4.2
         *
         *       0 1 2 3 4 5 6 7
         *      +-+-+-+-+-+-+-+-+
         *      |X|R|N|S|R| PID | (REQUIRED)
         *      +-+-+-+-+-+-+-+-+
         * X:   |I|L|T|K| RSV   | (OPTIONAL)
         *      +-+-+-+-+-+-+-+-+
         * I:   |M| PictureID   | (OPTIONAL)
         *      +-+-+-+-+-+-+-+-+
         * L:   |   tl0picidx   | (OPTIONAL)
         *      +-+-+-+-+-+-+-+-+
         * T/K: |tid|Y| KEYIDX  | (OPTIONAL)
         *      +-+-+-+-+-+-+-+-+
         *  S: Start of VP8 partition.  SHOULD be set to 1 when the first payload
         *     octet of the RTP packet is the beginning of a new VP8 partition,
         *     and MUST NOT be 1 otherwise.  The S bit MUST be set to 1 for the
         *     first packet of each encoded frame.
         */
        let using_header_size = if self.enable_picture_id {
            if self.picture_id == 0 || self.picture_id < 128 {
                VP8_HEADER_SIZE + 2
            } else {
                VP8_HEADER_SIZE + 3
            }
        } else {
            VP8_HEADER_SIZE
        };

        let max_fragment_size = mtu as isize - using_header_size as isize;
        let mut payload_data_remaining = payload.len() as isize;
        let mut payload_data_index: usize = 0;
        let mut payloads = vec![];

        // Make sure the fragment/payload size is correct
        if std::cmp::min(max_fragment_size, payload_data_remaining) <= 0 {
            return Ok(payloads);
        }

        let mut first = true;
        while payload_data_remaining > 0 {
            let current_fragment_size =
                std::cmp::min(max_fragment_size, payload_data_remaining) as usize;
            let mut out = Vec::with_capacity(using_header_size + current_fragment_size);
            let mut buf = [0u8; 4];
            if first {
                buf[0] = 0x10;
                first = false;
            }

            if self.enable_picture_id {
                if using_header_size == VP8_HEADER_SIZE + 2 {
                    buf[0] |= 0x80;
                    buf[1] |= 0x80;
                    buf[2] |= (self.picture_id & u16::from(PICTURE_ID_SHORT_MASK)) as u8;
                } else if using_header_size == VP8_HEADER_SIZE + 3 {
                    buf[0] |= 0x80;
                    buf[1] |= 0x80;
                    buf[2] |=
                        0x80 | ((self.picture_id >> 8) & u16::from(PICTURE_ID_SHORT_MASK)) as u8;
                    buf[3] |= (self.picture_id & 0xFF) as u8;
                }
            }

            out.extend_from_slice(&buf[..using_header_size]);

            out.extend_from_slice(
                &payload[payload_data_index..payload_data_index + current_fragment_size],
            );
            payloads.push(out);

            payload_data_remaining -= current_fragment_size as isize;
            payload_data_index += current_fragment_size;
        }

        self.picture_id += 1;
        self.picture_id &= PICTURE_ID_MAX;

        Ok(payloads)
    }

    fn is_marker(&mut self, _data: &[u8], _previous: Option<&[u8]>, last: bool) -> bool {
        last
    }
}

/// Depacketizes VP8 RTP packets.
///
/// ## Unversioned API surface
///
/// This struct is not currently versioned according to semver rules.
/// Breaking changes may be made in minor or patch releases.
#[derive(PartialEq, Eq, Debug, Default, Clone)]
pub struct Vp8Depacketizer {
    /// Required Header
    /// extended controlbits present
    pub x: u8,
    /// when set to 1 this frame can be discarded
    pub n: u8,
    /// start of VP8 partition
    pub s: u8,
    /// partition index
    pub pid: u8,

    /// Extended control bits
    /// 1 if PictureID is present
    pub i: u8,
    /// 1 if tl0picidx is present
    pub l: u8,
    /// 1 if tid is present
    pub t: u8,
    /// 1 if KEYIDX is present
    pub k: u8,

    /// Optional extension
    /// 8 or 16 bits, picture ID
    pub picture_id: u16,
    /// extended picture id
    pub extended_pid: Option<u64>,

    /// 8 bits temporal level zero index
    pub tl0_pic_idx: u8,
    /// extended version of picture_id of temporal layer 0
    pub extended_tl0_pic_idx: Option<u64>,
    /// 2 bits temporal layer index
    pub tid: u8,
    /// 1 bit layer sync bit
    pub y: u8,
    /// 5 bits temporal key frame index
    pub key_idx: u8,

    /// Inverse key frame flag.
    ///
    /// 0 if the current frame is a key frame.
    pub p: u8,
}

impl Depacketizer for Vp8Depacketizer {
    fn out_size_hint(&self, packets_size: usize) -> Option<usize> {
        Some(packets_size)
    }

    /// depacketize parses the passed byte slice and stores the result in the
    /// VP8Packet this method is called upon
    fn depacketize(
        &mut self,
        packet: &[u8],
        out: &mut Vec<u8>,
        extra: &mut CodecExtra,
    ) -> Result<(), PacketError> {
        let payload_len = packet.len();
        // VP8 Payload Descriptor
        // https://datatracker.ietf.org/doc/html/rfc7741#section-4.2
        //
        //    0 1 2 3 4 5 6 7                      0 1 2 3 4 5 6 7
        //    +-+-+-+-+-+-+-+-+                   +-+-+-+-+-+-+-+-+
        //    |X|R|N|S|R| PID | (REQUIRED)        |X|R|N|S|R| PID | (REQUIRED)
        //    +-+-+-+-+-+-+-+-+                   +-+-+-+-+-+-+-+-+
        // X: |I|L|T|K| RSV   | (OPTIONAL)   X:   |I|L|T|K| RSV   | (OPTIONAL)
        //    +-+-+-+-+-+-+-+-+                   +-+-+-+-+-+-+-+-+
        // I: |M| PictureID   | (OPTIONAL)   I:   |M| PictureID   | (OPTIONAL)
        //    +-+-+-+-+-+-+-+-+                   +-+-+-+-+-+-+-+-+
        // L: |   tl0picidx   | (OPTIONAL)        |   PictureID   |
        //    +-+-+-+-+-+-+-+-+                   +-+-+-+-+-+-+-+-+
        //T/K:|tid|Y| KEYIDX  | (OPTIONAL)   L:   |   tl0picidx   | (OPTIONAL)
        //    +-+-+-+-+-+-+-+-+                   +-+-+-+-+-+-+-+-+
        //                                    T/K:|tid|Y| KEYIDX  | (OPTIONAL)
        //                                        +-+-+-+-+-+-+-+-+

        let mut reader = (packet, 0);
        let mut payload_index = 0;

        let mut b = reader.get_u8().ok_or(PacketError::ErrShortPacket)?;
        payload_index += 1;

        self.x = (b & 0x80) >> 7;
        self.n = (b & 0x20) >> 5;
        self.s = (b & 0x10) >> 4;
        self.pid = b & 0x07;

        let (i, l, t, k) = if self.x == 1 {
            b = reader.get_u8().ok_or(PacketError::ErrShortPacket)?;
            payload_index += 1;
            let i = (b & 0x80) >> 7;
            let l = (b & 0x40) >> 6;
            let t = (b & 0x20) >> 5;
            let k = (b & 0x10) >> 4;

            if l == 1 && t == 0 {
                return Err(PacketError::ErrVP8CorruptedPacket);
            }

            (i, l, t, k)
        } else {
            (0, 0, 0, 0)
        };
        self.i = i;
        self.l = l;
        self.t = t;
        self.k = k;

        if self.i == 1 {
            b = reader.get_u8().ok_or(PacketError::ErrShortPacket)?;
            payload_index += 1;
            let previous_picture_id = u64::from(self.picture_id);
            let previous = self.extended_pid.map(|_| previous_picture_id);
            // PID present?
            let next = if b & 0x80 > 0 {
                // M == 1, PID is 16bit
                let x = reader.get_u8().ok_or(PacketError::ErrShortPacket)?;
                self.picture_id = (((b & PICTURE_ID_SHORT_MASK) as u16) << 8) | (x as u16);
                payload_index += 1;
                extend_u15(previous, self.picture_id)
            } else {
                self.picture_id = b as u16;
                extend_u7(previous, b)
            };

            // "The receiver MUST NOT assume that the number of bits in PictureID stays the
            // same through the session" (RFC 7741 section 4.2). Preserve the offset from
            // earlier short-ID wraps by extending the raw-ID delta.
            self.extended_pid = Some(match self.extended_pid {
                Some(previous) => previous.wrapping_add(next.wrapping_sub(previous_picture_id)),
                None => next,
            });
        }

        if payload_index >= payload_len {
            return Err(PacketError::ErrShortPacket);
        }

        if self.l == 1 {
            self.tl0_pic_idx = reader.get_u8().ok_or(PacketError::ErrShortPacket)?;
            self.extended_tl0_pic_idx =
                Some(extend_u8(self.extended_tl0_pic_idx, self.tl0_pic_idx));
            payload_index += 1;
        }

        if payload_index >= payload_len {
            return Err(PacketError::ErrShortPacket);
        }

        if self.t == 1 || self.k == 1 {
            let b = reader.get_u8().ok_or(PacketError::ErrShortPacket)?;
            if self.t == 1 {
                self.tid = b >> 6;
                self.y = (b >> 5) & 0x1;
            }
            if self.k == 1 {
                self.key_idx = b & KEY_IDX_MASK;
            }
            payload_index += 1;
        }

        if payload_index >= packet.len() {
            return Err(PacketError::ErrShortPacket);
        }

        out.extend_from_slice(&packet[payload_index..]);

        // VP8 Payload Header
        // https://datatracker.ietf.org/doc/html/rfc7741#section-4.3
        //
        //  0 1 2 3 4 5 6 7
        // +-+-+-+-+-+-+-+-+
        // |Size0|H| VER |P|
        // +-+-+-+-+-+-+-+-+
        // |     Size1     |
        // +-+-+-+-+-+-+-+-+
        // |     Size2     |
        // +-+-+-+-+-+-+-+-+
        // | Octets 4..N of|
        // | VP8 payload   |
        // :               :
        // +-+-+-+-+-+-+-+-+
        // | OPTIONAL RTP  |
        // | padding       |
        // :               :
        // +-+-+-+-+-+-+-+-+
        //
        // The header is present only in packets that have the S bit equal
        // to one and the PID equal to zero in the payload descriptor
        self.p = if self.s == 1 && self.pid == 0 {
            b = reader.get_u8().ok_or(PacketError::ErrShortPacket)?;
            payload_index += 1;
            b & 1
        } else {
            1
        };

        let is_keyframe = if let CodecExtra::Vp8(e) = extra {
            e.is_keyframe | (self.p == 0)
        } else {
            self.p == 0
        };

        *extra = CodecExtra::Vp8(Vp8CodecExtra {
            discardable: self.n == 1,
            sync: self.y == 1,
            layer_index: self.tid,
            picture_id: if self.i == 1 { self.extended_pid } else { None },
            tl0_picture_id: if self.l == 1 {
                self.extended_tl0_pic_idx
            } else {
                None
            },
            is_keyframe,
        });

        let _ = payload_index;

        Ok(())
    }

    /// is_partition_head checks whether if this is a head of the VP8 partition
    fn is_partition_head(&self, payload: &[u8]) -> bool {
        if payload.is_empty() {
            false
        } else {
            (payload[0] & 0x10) != 0
        }
    }

    fn is_partition_tail(&self, marker: bool, _payload: &[u8]) -> bool {
        marker
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn test_vp8_unmarshal() -> Result<(), PacketError> {
        let mut pck = Vp8Depacketizer::default();
        let mut extra = CodecExtra::None;

        // Empty packet
        let empty_bytes = &[];
        let mut payload = Vec::new();
        let result = pck.depacketize(empty_bytes, &mut payload, &mut extra);
        assert!(result.is_err(), "Result should be err in case of error");

        // Small Payload with single octet header
        let small_bytes = &[0x00, 0x11, 0x22];
        let mut payload = Vec::new();
        pck.depacketize(small_bytes, &mut payload, &mut extra)
            .expect("Small packet");
        assert_eq!(payload, [0x11, 0x22]);

        // Payload is header only
        let small_bytes = &[0x00];
        let mut payload = Vec::new();
        let result = pck.depacketize(small_bytes, &mut payload, &mut extra);
        assert!(result.is_err(), "Result should be err in case of error");

        // Normal packet
        let raw_bytes = &[0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x90];
        let mut payload = Vec::new();
        pck.depacketize(raw_bytes, &mut payload, &mut extra)
            .expect("Normal packet");
        assert!(!payload.is_empty(), "Payload must be not empty");

        // Header size, only X
        let raw_bytes = &[0x80, 0x00, 0x00, 0x00];
        let mut payload = Vec::new();
        pck.depacketize(raw_bytes, &mut payload, &mut extra)
            .expect("Only X");
        assert!(!payload.is_empty(), "Payload must be not empty");
        assert_eq!(pck.x, 1, "X must be 1");
        assert_eq!(pck.i, 0, "I must be 0");
        assert_eq!(pck.l, 0, "L must be 0");
        assert_eq!(pck.t, 0, "T must be 0");
        assert_eq!(pck.k, 0, "K must be 0");
        assert_eq!(pck.p, 1, "P must be 1");

        // Header size, X and I, PID 16bits
        let raw_bytes = &[0x80, 0x80, 0x81, 0x00, 0x00];
        let mut payload = Vec::new();
        pck.depacketize(raw_bytes, &mut payload, &mut extra)
            .expect("X and I, PID 16bits");
        assert!(!payload.is_empty(), "Payload must be not empty");
        assert_eq!(pck.x, 1, "X must be 1");
        assert_eq!(pck.i, 1, "I must be 1");
        assert_eq!(pck.l, 0, "L must be 0");
        assert_eq!(pck.t, 0, "T must be 0");
        assert_eq!(pck.k, 0, "K must be 0");
        assert_eq!(pck.p, 1, "P must be 1");

        // Header size, X and L without T
        let raw_bytes = &[0x80, 0x40, 0x00, 0x00];
        let mut payload = Vec::new();
        assert_eq!(
            pck.depacketize(raw_bytes, &mut payload, &mut extra),
            Err(PacketError::ErrVP8CorruptedPacket)
        );

        let raw_bytes = &[0x00, 0x11, 0x22];
        let mut payload = Vec::new();
        pck.depacketize(raw_bytes, &mut payload, &mut extra)
            .expect("valid packet after malformed descriptor");
        assert_eq!(payload, [0x11, 0x22]);

        // Header size, X and T
        let raw_bytes = &[0x80, 0x20, 0x00, 0x00];
        let mut payload = Vec::new();
        pck.depacketize(raw_bytes, &mut payload, &mut extra)
            .expect("X and T");
        assert!(!payload.is_empty(), "Payload must be not empty");
        assert_eq!(pck.x, 1, "X must be 1");
        assert_eq!(pck.i, 0, "I must be 0");
        assert_eq!(pck.l, 0, "L must be 0");
        assert_eq!(pck.t, 1, "T must be 1");
        assert_eq!(pck.k, 0, "K must be 0");
        assert_eq!(pck.p, 1, "P must be 1");

        // Header size, X and K
        let raw_bytes = &[0x80, 0x10, 0x00, 0x00];
        let mut payload = Vec::new();
        pck.depacketize(raw_bytes, &mut payload, &mut extra)
            .expect("X and K");
        assert!(!payload.is_empty(), "Payload must be not empty");
        assert_eq!(pck.x, 1, "X must be 1");
        assert_eq!(pck.i, 0, "I must be 0");
        assert_eq!(pck.l, 0, "L must be 0");
        assert_eq!(pck.t, 0, "T must be 0");
        assert_eq!(pck.k, 1, "K must be 1");
        assert_eq!(pck.p, 1, "P must be 1");

        // Header size, all flags and 8bit picture_id
        let raw_bytes = &[0xff, 0xff, 0x00, 0x00, 0x00, 0x00];
        let mut payload = Vec::new();
        pck.depacketize(raw_bytes, &mut payload, &mut extra)
            .expect("all flags and 8bit picture_id");
        assert!(!payload.is_empty(), "Payload must be not empty");
        assert_eq!(pck.x, 1, "X must be 1");
        assert_eq!(pck.i, 1, "I must be 1");
        assert_eq!(pck.l, 1, "L must be 1");
        assert_eq!(pck.t, 1, "T must be 1");
        assert_eq!(pck.k, 1, "K must be 1");
        assert_eq!(pck.p, 1, "P must be 1");

        // Header size, all flags and 16bit picture_id
        let raw_bytes = &[0xff, 0xff, 0x80, 0x00, 0x00, 0x00, 0x00];
        let mut payload = Vec::new();
        pck.depacketize(raw_bytes, &mut payload, &mut extra)
            .expect("all flags and 16bit picture_id");
        assert!(!payload.is_empty(), "Payload must be not empty");
        assert_eq!(pck.x, 1, "X must be 1");
        assert_eq!(pck.i, 1, "I must be 1");
        assert_eq!(pck.l, 1, "L must be 1");
        assert_eq!(pck.t, 1, "T must be 1");
        assert_eq!(pck.k, 1, "K must be 1");
        assert_eq!(pck.p, 1, "P must be 1");

        // Header size, X, I and P
        let raw_bytes = &[0x90, 0x80, 0x11, 0x10, 0x00, 0x00, 0x00];
        let mut payload = Vec::new();
        pck.depacketize(raw_bytes, &mut payload, &mut extra)
            .expect("all flags and 16bit picture_id");
        assert!(!payload.is_empty(), "Payload must be not empty");
        assert_eq!(pck.x, 1, "X must be 1");
        assert_eq!(pck.i, 1, "I must be 1");
        assert_eq!(pck.l, 0, "L must be 0");
        assert_eq!(pck.t, 0, "T must be 0");
        assert_eq!(pck.k, 0, "K must be 0");
        assert_eq!(pck.p, 0, "P must be 0");

        Ok(())
    }

    #[test]
    fn test_vp8_payload() -> Result<(), PacketError> {
        let tests: Vec<(&str, Vp8Packetizer, usize, Vec<&[u8]>, Vec<Vec<&[u8]>>)> = vec![
            (
                "WithoutPictureID",
                Vp8Packetizer::default(),
                2,
                vec![&[0x90, 0x90, 0x90], &[0x91, 0x91]],
                vec![
                    vec![&[0x10, 0x90], &[0x00, 0x90], &[0x00, 0x90]],
                    vec![&[0x10, 0x91], &[0x00, 0x91]],
                ],
            ),
            (
                "WithPictureID_1byte",
                Vp8Packetizer {
                    enable_picture_id: true,
                    picture_id: 0x20,
                },
                5,
                vec![&[0x90, 0x90, 0x90], &[0x91, 0x91]],
                vec![
                    vec![&[0x90, 0x80, 0x20, 0x90, 0x90], &[0x80, 0x80, 0x20, 0x90]],
                    vec![&[0x90, 0x80, 0x21, 0x91, 0x91]],
                ],
            ),
            (
                "WithPictureID_2bytes",
                Vp8Packetizer {
                    enable_picture_id: true,
                    picture_id: 0x120,
                },
                6,
                vec![&[0x90, 0x90, 0x90], &[0x91, 0x91]],
                vec![
                    vec![
                        &[0x90, 0x80, 0x81, 0x20, 0x90, 0x90],
                        &[0x80, 0x80, 0x81, 0x20, 0x90],
                    ],
                    vec![&[0x90, 0x80, 0x81, 0x21, 0x91, 0x91]],
                ],
            ),
        ];

        for (name, mut pck, mtu, payloads, expected) in tests {
            for (i, payload) in payloads.iter().enumerate() {
                let actual = pck.packetize(mtu, payload)?;
                assert_eq!(expected[i], actual, "{name}: Generated packet[{i}] differs");
            }
        }

        Ok(())
    }

    #[test]
    fn test_vp8_payload_eror() -> Result<(), PacketError> {
        let mut pck = Vp8Packetizer::default();
        let empty = &[];
        let payload = &[0x90, 0x90, 0x90];

        // Positive MTU, empty payload
        let result = pck.packetize(1, empty)?;
        assert!(result.is_empty(), "Generated payload should be empty");

        // Positive MTU, small payload
        let result = pck.packetize(1, payload)?;
        assert_eq!(result.len(), 0, "Generated payload should be empty");

        // Positive MTU, small payload
        let result = pck.packetize(2, payload)?;
        assert_eq!(
            result.len(),
            payload.len(),
            "Generated payload should be the same size as original payload size"
        );

        Ok(())
    }

    #[test]
    fn test_vp8_partition_head_checker_is_partition_head() -> Result<(), PacketError> {
        let vp8 = Vp8Depacketizer::default();

        //"SmallPacket"
        assert!(
            !vp8.is_partition_head(&[0x00]),
            "Small packet should not be the head of a new partition"
        );

        //"SFlagON",
        assert!(
            vp8.is_partition_head(&[0x10, 0x00, 0x00, 0x00]),
            "Packet with S flag should be the head of a new partition"
        );

        //"SFlagOFF"
        assert!(
            !vp8.is_partition_head(&[0x00, 0x00, 0x00, 0x00]),
            "Packet without S flag should not be the head of a new partition"
        );

        Ok(())
    }

    fn rewrite_payload(
        payload: &[u8],
        patch: impl FnOnce(Vp8PatchBuilder) -> Vp8PatchBuilder,
    ) -> Result<Vec<u8>, Vp8PatchError> {
        let descriptor = Vp8Descriptor::parse(payload).expect("valid VP8 descriptor");
        let patch = patch(descriptor.patch()).build()?;

        let mut out = vec![0; payload.len()];
        patch.copy_to(payload, &mut out);
        Ok(out)
    }

    #[test]
    fn test_vp8_descriptor_exposes_fields() {
        let short = Vp8Descriptor::parse(&[0x90, 0x80, 0x42, 0x00]).unwrap();
        assert_eq!(short.picture_id(), Some(0x42));
        assert_eq!(short.tl0_pic_idx(), None);
        assert_eq!(short.key_idx(), None);

        let full = Vp8Descriptor::parse(&[0x90, 0xf0, 0x92, 0x34, 0x55, 0xa7, 0x00]).unwrap();
        assert_eq!(full.picture_id(), Some(0x1234));
        assert_eq!(full.tl0_pic_idx(), Some(0x55));
        assert_eq!(full.key_idx(), Some(0x07));
    }

    #[test]
    fn test_vp8_patch_updates_descriptor_fields() {
        assert_eq!(
            rewrite_payload(&[0x80, 0x80, 0x01, 0x90], |patch| {
                patch.picture_id(u16::from(PICTURE_ID_SHORT_MASK))
            })
            .unwrap(),
            [0x80, 0x80, PICTURE_ID_SHORT_MASK, 0x90]
        );
        assert_eq!(
            rewrite_payload(&[0x80, 0x80, 0x81, 0x20, 0x90], |patch| {
                patch.picture_id(0x0234)
            })
            .unwrap(),
            [0x80, 0x80, 0x82, 0x34, 0x90]
        );
        assert_eq!(
            rewrite_payload(&[0x80, 0x80, 0x81, 0x20, 0x90], |patch| {
                patch.picture_id(PICTURE_ID_MAX)
            })
            .unwrap(),
            [0x80, 0x80, 0xff, 0xff, 0x90]
        );
        assert_eq!(
            rewrite_payload(&[0x80, 0x60, 0x12, 0x00, 0x90], |patch| {
                patch.tl0_pic_idx(0xab)
            })
            .unwrap(),
            [0x80, 0x60, 0xab, 0x00, 0x90]
        );
        assert_eq!(
            rewrite_payload(&[0x80, 0x30, 0xaa, 0x90], |patch| {
                patch.key_idx(KEY_IDX_MASK)
            })
            .unwrap(),
            [0x80, 0x30, 0xbf, 0x90]
        );
    }

    #[test]
    fn test_vp8_patch_rejects_unsupported_descriptor_shapes() {
        assert_eq!(
            Vp8Descriptor::parse(&[0x80, 0x80]).unwrap_err(),
            Vp8DescriptorError::ShortPayload
        );
        assert_eq!(
            rewrite_payload(&[0x00, 0x90], |patch| patch.picture_id(1)).unwrap_err(),
            Vp8PatchError::PictureIdMissing
        );
        assert_eq!(
            rewrite_payload(&[0x00, 0x90], |patch| {
                patch.picture_id(PICTURE_ID_MAX + 1)
            })
            .unwrap_err(),
            Vp8PatchError::PictureIdMissing
        );
        assert_eq!(
            rewrite_payload(&[0x80, 0x80, 0x81, 0x20, 0x90], |patch| {
                patch.tl0_pic_idx(1)
            })
            .unwrap_err(),
            Vp8PatchError::Tl0PicIdxMissing
        );
        assert_eq!(
            rewrite_payload(&[0x80, 0x80, 0x81, 0x20, 0x90], |patch| {
                patch.key_idx(1)
            })
            .unwrap_err(),
            Vp8PatchError::KeyIdxMissing
        );
        assert_eq!(
            rewrite_payload(&[0x80, 0x80, 0x81, 0x20, 0x90], |patch| {
                patch.key_idx(KEY_IDX_MASK + 1)
            })
            .unwrap_err(),
            Vp8PatchError::KeyIdxMissing
        );
        assert_eq!(
            Vp8Descriptor::parse(&[0x80, 0x40, 0x12, 0x90]).unwrap_err(),
            Vp8DescriptorError::MalformedDescriptor
        );
    }

    #[test]
    fn test_vp8_patch_rejects_impossible_values() {
        assert_eq!(
            rewrite_payload(&[0x80, 0x80, 0x01, 0x90], |patch| {
                patch.picture_id(0x80)
            })
            .unwrap_err(),
            Vp8PatchError::PictureIdTooLarge
        );
        assert_eq!(
            rewrite_payload(&[0x80, 0x80, 0x81, 0x20, 0x90], |patch| {
                patch.picture_id(PICTURE_ID_MAX + 1)
            })
            .unwrap_err(),
            Vp8PatchError::PictureIdTooLarge
        );
        assert_eq!(
            rewrite_payload(&[0x80, 0x10, 0x00, 0x90], |patch| {
                patch.key_idx(KEY_IDX_MASK + 1)
            })
            .unwrap_err(),
            Vp8PatchError::KeyIdxTooLarge
        );
    }

    #[test]
    fn test_detect_vp8_keyframe() {
        // Empty payload
        assert!(!detect_vp8_keyframe(&[]));

        // Minimal keyframe: S=1, PID=0, no extensions, P=0
        // Byte 0: X=0, R=0, N=0, S=1, PID=0 → 0x10
        // Byte 1: VP8 payload header with P=0 (keyframe) → 0x00
        assert!(detect_vp8_keyframe(&[0x10, 0x00]));

        // Minimal interframe: S=1, PID=0, no extensions, P=1
        // Byte 1: VP8 payload header with P=1 → 0x01
        assert!(!detect_vp8_keyframe(&[0x10, 0x01]));

        // Not the first packet (S=0) — cannot detect keyframe
        assert!(!detect_vp8_keyframe(&[0x00, 0x00]));

        // Continuation packet (PID != 0)
        assert!(!detect_vp8_keyframe(&[0x11, 0x00]));

        // With extension (X=1), 7-bit PictureID, keyframe
        // Byte 0: X=1, S=1, PID=0 → 0x90
        // Byte 1: I=1, L=0, T=0, K=0 → 0x80
        // Byte 2: 7-bit PictureID (M=0) → 0x42
        // Byte 3: VP8 payload header P=0 → 0x00
        assert!(detect_vp8_keyframe(&[0x90, 0x80, 0x42, 0x00]));

        // With extension, 7-bit PictureID, interframe
        assert!(!detect_vp8_keyframe(&[0x90, 0x80, 0x42, 0x01]));

        // With extension, 16-bit PictureID (M=1), keyframe
        // Byte 2: M=1 → 0x80 | PID_high
        // Byte 3: PID_low
        // Byte 4: VP8 payload header P=0
        assert!(detect_vp8_keyframe(&[0x90, 0x80, 0x80, 0x42, 0x00]));

        // With all extensions: I=1(16-bit), L=1, T=1
        // Byte 0: X=1, S=1 → 0x90
        // Byte 1: I=1, L=1, T=1 → 0xE0
        // Byte 2-3: 16-bit PictureID → 0x80, 0x42
        // Byte 4: TL0PICIDX
        // Byte 5: TID/KEYIDX
        // Byte 6: VP8 payload header P=0
        assert!(detect_vp8_keyframe(&[
            0x90, 0xE0, 0x80, 0x42, 0x01, 0x00, 0x00
        ]));

        assert!(detect_vp8_keyframe(&[0x90, 0x40, 0x01, 0x00]));

        // Truncated: extension says PictureID but no bytes left
        assert!(!detect_vp8_keyframe(&[0x90, 0x80]));

        // Truncated: header consumed all bytes
        assert!(!detect_vp8_keyframe(&[0x90, 0x80, 0x42]));
    }

    #[test]
    fn test_vp8_descriptor_keyframe_detection_matches_helper() {
        let payloads: &[&[u8]] = &[
            &[],
            &[0x10, 0x00],
            &[0x10, 0x01],
            &[0x00, 0x00],
            &[0x11, 0x00],
            &[0x90, 0x80, 0x42, 0x00],
            &[0x90, 0x80, 0x42, 0x01],
            &[0x90, 0x80],
            &[0x90, 0x80, 0x42],
        ];

        for payload in payloads {
            let descriptor_starts_keyframe = Vp8Descriptor::parse(payload)
                .map(|descriptor| descriptor.starts_keyframe(payload))
                .unwrap_or(false);
            assert_eq!(descriptor_starts_keyframe, detect_vp8_keyframe(payload));
        }
    }

    #[test]
    fn packetize_respects_mtu() -> Result<(), PacketError> {
        let payload = vec![0xABu8; 2000];
        for &mtu in &[100usize, 300, 600, 1200] {
            let mut pck = Vp8Packetizer::default();
            let pkts = pck.packetize(mtu, &payload)?;
            assert!(!pkts.is_empty(), "VP8 produced no packets at mtu {mtu}");
            for (i, pkt) in pkts.iter().enumerate() {
                assert!(
                    pkt.len() <= mtu,
                    "VP8 packet {i} size {} > mtu {mtu}",
                    pkt.len()
                );
            }
        }
        Ok(())
    }

    fn picture_id_of(depack: &mut Vp8Depacketizer, picture_id: &[u8]) -> Option<u64> {
        // X=1 S=1, I=1, picture id, inter frame
        let mut packet = vec![0x90, 0x80];
        packet.extend_from_slice(picture_id);
        packet.push(0x01);
        let mut payload = Vec::new();
        let mut extra = CodecExtra::None;
        depack
            .depacketize(&packet, &mut payload, &mut extra)
            .unwrap();
        let CodecExtra::Vp8(extra) = extra else {
            panic!("expected VP8 codec extra");
        };
        extra.picture_id
    }

    /// X=1 S=1, I=1 L=1 T=1, picture id, tl0picidx, TID=0
    fn frame(keyframe: bool, picture_id: &[u8], tl0_pic_idx: u8) -> Vec<Vec<u8>> {
        let mut packet = vec![0x90, 0xE0];
        packet.extend_from_slice(picture_id);
        packet.extend_from_slice(&[tl0_pic_idx, 0x00, u8::from(!keyframe)]);
        vec![packet]
    }

    fn assert_every_frame_emitted(frames: impl IntoIterator<Item = Vec<Vec<u8>>>) {
        use crate::packet::{CodecDepacketizer, DepacketizingBuffer, RtpMeta};
        use crate::rtp_::{MediaTime, RtpHeader};
        use std::time::Instant;

        let mut buf =
            DepacketizingBuffer::new(CodecDepacketizer::Vp8(Vp8Depacketizer::default()), 3);
        let now = Instant::now();
        let mut seq = 0_u64;

        for (frame, packets) in frames.into_iter().enumerate() {
            let count = packets.len();
            let time = frame as u64 * 3000;

            for (i, packet) in packets.into_iter().enumerate() {
                let meta = RtpMeta {
                    received: now,
                    time: MediaTime::from_90khz(time),
                    seq_no: seq.into(),
                    last_sender_info: None,
                    header: RtpHeader {
                        sequence_number: seq as u16,
                        timestamp: time as u32,
                        marker: i == count - 1,
                        ..Default::default()
                    },
                };
                buf.push(meta, packet);
                seq += 1;
            }

            let dep = buf
                .pop(now, None)
                .unwrap_or_else(|| panic!("frame {frame} was not emitted"))
                .unwrap();
            assert!(dep.contiguous, "frame {frame} is not contiguous");
        }
    }

    #[test]
    fn depacketizer_preserves_picture_id_epoch_when_width_grows() {
        let mut depack = Vp8Depacketizer::default();
        for (picture_id, expected) in [(126_u8, 126_u64), (127, 127), (0, 128), (1, 129)] {
            assert_eq!(picture_id_of(&mut depack, &[picture_id]), Some(expected));
        }
        // The 15-bit PictureID continues incrementing from the 7-bit one.
        assert_eq!(picture_id_of(&mut depack, &[0x80, 2]), Some(130));
    }

    #[test]
    fn depacketizing_buffer_emits_frames_when_picture_id_width_grows() {
        let frames = [
            frame(true, &[126], 10),
            frame(false, &[127], 11),
            frame(false, &[0], 12),
            frame(false, &[1], 13),
            frame(false, &[0x80, 2], 14),
            frame(false, &[0x80, 3], 15),
        ];
        assert_every_frame_emitted(frames);
    }

    #[test]
    fn depacketizer_preserves_wide_picture_id_after_loss_at_width_growth() {
        let mut depack = Vp8Depacketizer::default();
        assert_eq!(picture_id_of(&mut depack, &[0]), Some(0));
        // The sender widens at PictureID 1; pictures 1..99 are lost in transit.
        assert_eq!(picture_id_of(&mut depack, &[0x80, 100]), Some(100));
    }

    #[test]
    fn depacketizing_buffer_recovers_after_loss_at_picture_id_width_growth() {
        // A middlebox drops pictures 1..99 and rewrites RTP sequence numbers.
        // Picture 100 is a keyframe, so all following pictures are decodable.
        let frames = std::iter::once(frame(true, &[0], 0))
            .chain((100..=130_u8).map(|pid| frame(pid == 100, &[0x80, pid], pid - 99)));
        assert_every_frame_emitted(frames);
    }
}
