use super::{BitRead, CodecExtra, Depacketizer, PacketError, Packetizer};
use crate::rtp_::{extend_u7, extend_u8, extend_u15};

use std::fmt;

/// Max non-flexible VP9 descriptor size reserved for MTU headroom:
/// flags(1) + PID(2) + L(1) + tl0picidx(1) + ≤1 SS byte on keyframe first-packets; 8 is a safe bound.
const VP9_NON_FLEXIBLE_HEADER_SIZE: usize = 8;
/// VP9 RTP payload descriptor size for flexible mode:
/// flags(1) + 15-bit PID(2) = 3
const VP9_FLEXIBLE_HEADER_SIZE: usize = 3;
const MAX_SPATIAL_LAYERS: usize = 3;
const MAX_VP9REF_PICS: usize = 3;

/// VP9 packetizer mode controlling the RTP payload descriptor format.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum Vp9PacketizerMode {
    /// Flexible mode (F=1): minimal 3-byte header (flags + 15-bit PID).
    /// Simpler but causes issues with Safari which drops inter-frames.
    Flexible,
    /// Non-flexible mode (F=0): 5-6 byte header with layer indices and
    /// scalability structure. Compatible with all major browsers.
    #[default]
    NonFlexible,
}

/// Vp9 information describing the depacketized/packetized data.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Vp9CodecExtra {
    /// Map of the SVC layers.
    ///
    /// Index of each element corresponds to `spatial layer` index.
    ///
    /// Each element represents the end (not including) in [`MediaData::data`]
    /// for `spatial layer` it belongs.
    ///
    /// The all `spatial layer`s (up to 3) in [`MediaData::data`] are sorted by
    /// `spatial layer` index from lower to greater. So, the beginning of some
    /// `spatial layer` is the end (including) of the previous `spatial layer`
    /// (for the `spatial layer` with index 0 the beginning is 0).
    ///
    /// ```text
    /// [MediaData::data]:
    /// +-----+----------------------+---------------------------+
    /// | SL0 |         SL1          |            SL2            |
    /// +-----+----------------------+---------------------------+
    /// |     |                      |                           |
    /// V     V                      V                           V
    /// 0   layers_scheme[0]       layers_scheme[1]            layers_scheme[2]
    /// ```
    ///
    /// **Note:** There is no syncronization between
    /// [`Vp9CodecExtra::layers_scheme`] and [`MediaData::data`]! If you need
    /// to keep [`Vp9CodecExtra::layers_scheme`] in actual state you have to
    /// update the [`Vp9CodecExtra::layers_scheme`] on mutating the
    /// [`MediaData::data`].
    ///
    /// [`MediaData::data`]: crate::media::MediaData::data
    ///
    /// ## Example
    ///
    /// Say you have a VP9 track working in "L3T3" scalability mode and you
    /// want to retranslate S1T2 layer. So you need to retranslate the all
    /// spatial and temporal layers <= target layer.
    ///
    /// ```no_run
    /// # use str0m::{format::CodecExtra, media::MediaData};
    /// #
    /// // We want to receive S1T2 layer.
    /// let (target_spatial, target_temporal) = (1u8, 2u8);
    ///
    /// let media_data: MediaData = todo!();
    ///
    /// if let CodecExtra::Vp9(vp9_extra) = media_data.codec_extra {
    ///     // Try to get current `temporal layer`. If `None` there is no SVC.
    ///     if let Some(tid) = vp9_extra.tid {
    ///         if let (true, Some(end_of_target_layer)) = (
    ///             // Check whether we need this `temporal layer`.
    ///             tid <= target_temporal,
    ///             // Check whether we have information about the right
    ///             // border of data for the target `spatial layer`.
    ///             vp9_extra.layers_scheme[target_spatial as usize],
    ///         ) {
    ///             // Keep only the bytes for spatial layers up to `target_spatial`.
    ///             let data = &media_data.data[..end_of_target_layer];
    ///         }
    ///     }
    /// }
    /// ```
    pub layers_scheme: [Option<usize>; MAX_SPATIAL_LAYERS],

    /// Temporal layer id.
    pub tid: Option<u8>,

    /// Map of the SVC layers widths.
    ///
    /// Specified for every spatial layer.
    pub layers_widths: [Option<u16>; MAX_SPATIAL_LAYERS],

    /// Map of the SVC layers heights.
    ///
    /// Specified for every spatial layer.
    pub layers_heights: [Option<u16>; MAX_SPATIAL_LAYERS],

    /// Extended picture id of layer 0 frames, if present
    pub tl0_picture_id: Option<u64>,

    /// Extended picture id, if present
    pub picture_id: Option<u64>,

    /// Flag which indicates that within [`MediaData`], there is an individual frame
    /// containing complete and independent visual information. This frame serves
    /// as a reference point for other frames in the video sequence.
    ///
    /// [`MediaData`]: crate::media::MediaData
    pub is_keyframe: bool,
}

/// Detect whether a raw VP9 frame is a keyframe by inspecting the bitstream header.
///
/// VP9 uncompressed header (profile 0/1):
///   `frame_marker(2) | profile_low(1) | profile_high(1) | show_existing(1) | frame_type(1)`
///
/// VP9 uncompressed header (profile 2/3):
///   `frame_marker(2) | profile_low(1) | profile_high(1) | reserved(1) | show_existing(1) | frame_type(1)`
///
/// - `frame_marker` must be `0b10`
/// - `frame_type`: 0 = KEY_FRAME, 1 = NON_KEY_FRAME
/// - `show_existing_frame` = 1 references a previously decoded frame (not a real keyframe)
///
/// This function works on raw VP9 bitstream data (without RTP descriptor).
pub fn detect_vp9_keyframe_bitstream(payload: &[u8]) -> bool {
    if payload.is_empty() {
        return false;
    }
    let b = payload[0];
    // frame_marker must be 0b10 (bits 7-6)
    if (b & 0xC0) != 0x80 {
        return false;
    }
    // profile_high_bit is at bit 4
    let profile_high = (b >> 4) & 1;
    let (show_existing_frame, frame_type) = if profile_high == 1 {
        // Profile 2 or 3: reserved_zero at bit 3, show_existing at bit 2, frame_type at bit 1
        ((b >> 2) & 1, (b >> 1) & 1)
    } else {
        // Profile 0 or 1: show_existing at bit 3, frame_type at bit 2
        ((b >> 3) & 1, (b >> 2) & 1)
    };
    // show_existing_frame references a previously decoded frame, not a real keyframe
    if show_existing_frame != 0 {
        return false;
    }
    // frame_type: 0 = KEY_FRAME
    frame_type == 0
}

/// Detect whether a VP9 RTP payload contains a keyframe by inspecting the P bit.
///
/// VP9 RTP descriptor byte 0: `I|P|L|F|B|E|V|Z`
/// - P=0: independently decodable frame (keyframe)
/// - P=1: inter-picture predicted frame
///
/// Works with both flexible (F=1) and non-flexible (F=0) mode packets.
pub fn detect_vp9_keyframe(payload: &[u8]) -> bool {
    if payload.is_empty() {
        return false;
    }
    // P bit is bit 6 of byte 0. P=0 means keyframe.
    (payload[0] & 0x40) == 0
}

/// Packetizes VP9 RTP packets.
///
/// Supports both non-flexible mode (F=0) and flexible mode (F=1), controlled
/// by [`Vp9PacketizerMode`]. Defaults to non-flexible mode for broad browser
/// compatibility — Safari and older Chrome drop inter-frames with F=1.
#[derive(Default, Clone)]
pub struct Vp9Packetizer {
    picture_id: u16,
    /// Temporal layer 0 picture index — increments on every frame (since all
    /// frames are packetized as TID=0). Used in L byte for non-flexible mode.
    tl0picidx: u8,
    initialized: bool,
    mode: Vp9PacketizerMode,
    #[cfg(test)]
    initial_picture_id: u16,
}

impl Vp9Packetizer {
    /// Create a VP9 packetizer with the specified mode.
    pub fn with_mode(mode: Vp9PacketizerMode) -> Self {
        Self {
            mode,
            ..Default::default()
        }
    }
}

impl fmt::Debug for Vp9Packetizer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Vp9Packetizer")
            .field("picture_id", &self.picture_id)
            .field("tl0picidx", &self.tl0picidx)
            .field("initialized", &self.initialized)
            .field("mode", &self.mode)
            .finish()
    }
}

impl Packetizer for Vp9Packetizer {
    /// Packetize a VP9 frame into one or more RTP packets.
    fn packetize(&mut self, mtu: usize, payload: &[u8]) -> Result<Vec<Vec<u8>>, PacketError> {
        if payload.is_empty() || mtu == 0 {
            return Ok(vec![]);
        }

        if !self.initialized {
            #[cfg(test)]
            {
                self.picture_id = self.initial_picture_id;
            }
            #[cfg(not(test))]
            {
                use str0m_proto::NonCryptographicRng;
                self.picture_id = NonCryptographicRng::u16() & 0x7FFF;
            }
            self.initialized = true;
        }

        match self.mode {
            Vp9PacketizerMode::NonFlexible => self.packetize_non_flexible(mtu, payload),
            Vp9PacketizerMode::Flexible => self.packetize_flexible(mtu, payload),
        }
    }

    fn is_marker(&mut self, _data: &[u8], _previous: Option<&[u8]>, last: bool) -> bool {
        last
    }
}

impl Vp9Packetizer {
    /// Non-flexible mode (F=0) packetization.
    ///
    /// 5-8 byte header with layer indices and scalability structure.
    /// Compatible with all major browsers including Safari.
    ///
    /// ```text
    ///        0 1 2 3 4 5 6 7
    ///       +-+-+-+-+-+-+-+-+
    ///       |I|P|L|F|B|E|V|Z| (REQUIRED)
    ///       +-+-+-+-+-+-+-+-+
    ///  I:   |M| PICTURE ID  | (RECOMMENDED)
    ///       +-+-+-+-+-+-+-+-+
    ///  M:   | EXTENDED PID  | (RECOMMENDED)
    ///       +-+-+-+-+-+-+-+-+
    ///  L:   | tid |U| SID |D| (CONDITIONALLY RECOMMENDED)
    ///       +-+-+-+-+-+-+-+-+
    ///       |   tl0picidx   | (CONDITIONALLY REQUIRED)
    ///       +-+-+-+-+-+-+-+-+
    ///  V:   | SS            |
    ///       | ..            |
    ///       +-+-+-+-+-+-+-+-+
    /// ```
    fn packetize_non_flexible(
        &mut self,
        mtu: usize,
        payload: &[u8],
    ) -> Result<Vec<Vec<u8>>, PacketError> {
        // Detect keyframe from VP9 bitstream to set P flag correctly.
        let is_keyframe = detect_vp9_keyframe_bitstream(payload);

        // tl0picidx increments on every TID=0 frame. Since we always emit TID=0,
        // this increments on every frame per draft-ietf-payload-vp9, Section 6.3.
        self.tl0picidx = self.tl0picidx.wrapping_add(1);

        // VP9_NON_FLEXIBLE_HEADER_SIZE is the max header (8 bytes, accounting for SS
        // on keyframes). This ensures we never exceed MTU even on keyframe first-packets.
        let max_fragment_size = mtu as isize - VP9_NON_FLEXIBLE_HEADER_SIZE as isize;
        let mut payloads = vec![];
        let mut payload_data_remaining = payload.len();
        let mut payload_data_index = 0;

        if std::cmp::min(max_fragment_size, payload_data_remaining as isize) <= 0 {
            return Ok(vec![]);
        }

        while payload_data_remaining > 0 {
            let current_fragment_size =
                std::cmp::min(max_fragment_size as usize, payload_data_remaining);
            let is_first = payload_data_index == 0;
            let is_last = payload_data_remaining == current_fragment_size;

            // Non-keyframe: 5 bytes (flags + 15-bit PID + L + tl0picidx)
            // Keyframe first packet: 6 bytes (+ 1 byte SS data, G=0)
            let ss_size = if is_keyframe && is_first { 1 } else { 0 };
            let header_size = 5 + ss_size;
            let mut out = Vec::with_capacity(header_size + current_fragment_size);

            // Byte 0: I|P|L|F|B|E|V|Z
            let mut flags = 0xA0u8; // I=1, L=1 (F=0 implicit)
            if !is_keyframe {
                flags |= 0x40; // P=1 for inter-predicted frames
            }
            if is_first {
                flags |= 0x08; // B=1 (beginning of frame)
            }
            if is_last {
                flags |= 0x04; // E=1 (end of frame)
            }
            if is_keyframe && is_first {
                flags |= 0x02; // V=1 (scalability structure present)
            }
            out.push(flags);

            // Bytes 1-2: 15-bit picture ID (M=1)
            out.push((self.picture_id >> 8) as u8 | 0x80);
            out.push((self.picture_id & 0xFF) as u8);

            // Bytes 3-4: Layer indices (L=1, non-flexible mode)
            // TID(3)|U(1)|SID(3)|D(1) = TID=0, U=1, SID=0, D=0
            out.push(0x10);
            // tl0picidx (required when F=0 and L=1)
            out.push(self.tl0picidx);

            // SS byte on keyframe first packet: N_S=0, Y=0, G=0 (no GOF). A single-layer stream has no
            // GOF to advertise; a G=1/R=0 GOF mis-signals references (see commit msg). This matches what
            // libwebrtc emits for non-scalable VP9 (g_bit = num_frames_in_gof > 0). RFC 9628 §4.2.
            if is_keyframe && is_first {
                out.push(0x00);
            }

            out.extend_from_slice(
                &payload[payload_data_index..payload_data_index + current_fragment_size],
            );

            payloads.push(out);

            payload_data_remaining -= current_fragment_size;
            payload_data_index += current_fragment_size;
        }

        self.picture_id += 1;
        self.picture_id &= 0x7FFF;

        Ok(payloads)
    }

    /// Flexible mode (F=1) packetization.
    ///
    /// Minimal 3-byte header (flags + 15-bit PID). Simpler but may cause
    /// issues with Safari which drops inter-frames.
    ///
    /// ```text
    ///        0 1 2 3 4 5 6 7
    ///       +-+-+-+-+-+-+-+-+
    ///       |I|P|L|F|B|E|V|Z| (REQUIRED)
    ///       +-+-+-+-+-+-+-+-+
    ///  I:   |M| PICTURE ID  | (RECOMMENDED)
    ///       +-+-+-+-+-+-+-+-+
    ///  M:   | EXTENDED PID  | (RECOMMENDED)
    ///       +-+-+-+-+-+-+-+-+
    /// ```
    fn packetize_flexible(
        &mut self,
        mtu: usize,
        payload: &[u8],
    ) -> Result<Vec<Vec<u8>>, PacketError> {
        let max_fragment_size = mtu as isize - VP9_FLEXIBLE_HEADER_SIZE as isize;
        let mut payloads = vec![];
        let mut payload_data_remaining = payload.len();
        let mut payload_data_index = 0;

        if std::cmp::min(max_fragment_size, payload_data_remaining as isize) <= 0 {
            return Ok(vec![]);
        }

        while payload_data_remaining > 0 {
            let current_fragment_size =
                std::cmp::min(max_fragment_size as usize, payload_data_remaining);
            let is_first = payload_data_index == 0;
            let is_last = payload_data_remaining == current_fragment_size;

            let mut out = Vec::with_capacity(VP9_FLEXIBLE_HEADER_SIZE + current_fragment_size);

            // Byte 0: I|P|L|F|B|E|V|Z — I=1, F=1
            let mut flags = 0x90u8; // I=1, F=1
            if is_first {
                flags |= 0x08; // B=1
            }
            if is_last {
                flags |= 0x04; // E=1
            }
            out.push(flags);

            // Bytes 1-2: 15-bit picture ID (M=1)
            out.push((self.picture_id >> 8) as u8 | 0x80);
            out.push((self.picture_id & 0xFF) as u8);

            out.extend_from_slice(
                &payload[payload_data_index..payload_data_index + current_fragment_size],
            );

            payloads.push(out);

            payload_data_remaining -= current_fragment_size;
            payload_data_index += current_fragment_size;
        }

        self.picture_id += 1;
        self.picture_id &= 0x7FFF;

        Ok(payloads)
    }
}

/// Depacketizes VP9 RTP packets.
#[derive(PartialEq, Eq, Debug, Default, Clone)]
pub struct Vp9Depacketizer {
    /// picture ID is present
    pub i: bool,
    /// inter-picture predicted frame.
    pub p: bool,
    /// layer indices present
    pub l: bool,
    /// flexible mode
    pub f: bool,
    /// start of frame. beginning of new vp9 frame
    pub b: bool,
    /// end of frame
    pub e: bool,
    /// scalability structure (SS) present
    pub v: bool,
    /// Not a reference frame for upper spatial layers
    pub z: bool,

    /// Recommended headers
    /// 7 or 15 bits, picture ID.
    pub picture_id: u16,
    /// extended picture id
    pub extended_pid: Option<u64>,

    /// Conditionally recommended headers
    /// Temporal layer ID
    pub tid: u8,
    /// Switching up point
    pub u: bool,
    /// Spatial layer ID
    pub sid: u8,
    /// Inter-layer dependency used
    pub d: bool,

    /// Conditionally required headers
    /// Reference index (F=1)
    pub pdiff: [u8; MAX_VP9REF_PICS],
    /// Temporal layer zero index (F=0)
    pub tl0picidx: u8,
    /// extended version of tl0picidx
    pub extended_tl0picidx: Option<u64>,

    /// Scalability structure headers
    /// N_S + 1 indicates the number of spatial layers present in the VP9 stream
    pub ns: u8,
    /// Each spatial layer's frame resolution present
    pub y: bool,
    /// PG description present flag.
    pub g: bool,
    /// N_G indicates the number of pictures in a Picture Group (PG)
    pub ng: u8,
    pub width: [Option<u16>; MAX_SPATIAL_LAYERS],
    pub height: [Option<u16>; MAX_SPATIAL_LAYERS],
    /// Temporal layer ID of pictures in a Picture Group
    pub pgtid: Vec<u8>,
    /// Switching up point of pictures in a Picture Group
    pub pgu: Vec<bool>,
    /// Reference indices of pictures in a Picture Group
    pub pgpdiff: Vec<Vec<u8>>,
}

impl Depacketizer for Vp9Depacketizer {
    fn out_size_hint(&self, packets_size: usize) -> Option<usize> {
        Some(packets_size)
    }

    /// depacketize parses the passed byte slice and stores the result
    /// in the Vp9Packet this method is called upon
    fn depacketize(
        &mut self,
        packet: &[u8],
        out: &mut Vec<u8>,
        extra: &mut CodecExtra,
    ) -> Result<(), PacketError> {
        if packet.is_empty() {
            return Err(PacketError::ErrShortPacket);
        }

        let mut reader = (packet, 0);
        let b = reader.get_u8().ok_or(PacketError::ErrShortPacket)?;

        self.i = (b & 0x80) != 0;
        self.p = (b & 0x40) != 0;
        self.l = (b & 0x20) != 0;
        self.f = (b & 0x10) != 0;
        self.b = (b & 0x08) != 0;
        self.e = (b & 0x04) != 0;
        self.v = (b & 0x02) != 0;
        self.z = (b & 0x01) != 0;

        let mut payload_index = 1;

        if self.i {
            payload_index = self.parse_picture_id(&mut reader, payload_index)?;
        }

        if self.l {
            payload_index = self.parse_layer_info(&mut reader, payload_index)?;
        }

        if self.f && self.p {
            payload_index = self.parse_ref_indices(&mut reader, payload_index)?;
        }

        if self.v {
            payload_index = self.parse_ssdata(&mut reader, payload_index)?;
        }

        self.update_extra(extra, out.len(), packet.len(), payload_index)?;

        out.extend_from_slice(&packet[payload_index..]);

        Ok(())
    }

    /// is_partition_head checks whether if this is a head of the VP9 partition
    fn is_partition_head(&self, payload: &[u8]) -> bool {
        if payload.is_empty() {
            false
        } else {
            (payload[0] & 0x08) != 0
        }
    }

    fn is_partition_tail(&self, marker: bool, _payload: &[u8]) -> bool {
        marker
    }
}

impl Vp9Depacketizer {
    /// Updates provided [`CodecExtra`].
    /// __MUST__ be called after the all transformations of `payload_index`.
    fn update_extra(
        &self,
        extra: &mut CodecExtra,
        out_len: usize,
        packet_len: usize,
        payload_index: usize,
    ) -> Result<(), PacketError> {
        let mut vp9_extra = match extra {
            CodecExtra::Vp9(e) => *e,
            _ => Vp9CodecExtra::default(),
        };

        if self.l {
            let new_stop = out_len + packet_len - payload_index;

            if let Some(stop) = vp9_extra.layers_scheme[self.sid as usize] {
                if stop != out_len {
                    return Err(PacketError::ErrVP9CorruptedPacket);
                }
            }

            vp9_extra.layers_scheme[self.sid as usize] = Some(new_stop);
            vp9_extra.tid = Some(self.tid);

            if !self.f {
                vp9_extra.tl0_picture_id = self.extended_tl0picidx;
            }
        }

        // I: "Picture ID (PID) present" (RFC 9628 section 4.2). A packet without it
        // keeps the picture id of an earlier packet of the same frame.
        if self.i {
            vp9_extra.picture_id = self.extended_pid;
        }
        vp9_extra
            .layers_widths
            .copy_from_slice(&self.width[..MAX_SPATIAL_LAYERS]);
        vp9_extra
            .layers_heights
            .copy_from_slice(&self.height[..MAX_SPATIAL_LAYERS]);
        vp9_extra.is_keyframe |= !self.p && (self.sid == 0 || !self.l) && self.b;

        *extra = CodecExtra::Vp9(vp9_extra);

        Ok(())
    }

    // Picture ID:
    //
    //      +-+-+-+-+-+-+-+-+
    // I:   |M| PICTURE ID  |   M:0 => picture id is 7 bits.
    //      +-+-+-+-+-+-+-+-+   M:1 => picture id is 15 bits.
    // M:   | EXTENDED PID  |
    //      +-+-+-+-+-+-+-+-+
    //
    fn parse_picture_id(
        &mut self,
        reader: &mut dyn BitRead,
        mut payload_index: usize,
    ) -> Result<usize, PacketError> {
        if reader.remaining_bits() == 0 {
            return Err(PacketError::ErrShortPacket);
        }
        let b = reader.get_u8().ok_or(PacketError::ErrShortPacket)?;
        payload_index += 1;
        let previous_picture_id = u64::from(self.picture_id);
        let previous = self.extended_pid.map(|_| previous_picture_id);
        // PID present?
        let next = if (b & 0x80) != 0 {
            if reader.remaining_bits() == 0 {
                return Err(PacketError::ErrShortPacket);
            }
            // M == 1, PID is 15bit
            let x = reader.get_u8().ok_or(PacketError::ErrShortPacket)?;
            self.picture_id = (((b & 0x7f) as u16) << 8) | (x as u16);
            payload_index += 1;
            extend_u15(previous, self.picture_id)
        } else {
            self.picture_id = (b & 0x7F) as u16;
            extend_u7(previous, b)
        };

        // A wider wire ID is zero-extended after a width change. Preserve the
        // offset from earlier short-ID wraps by extending the raw-ID delta.
        self.extended_pid = Some(match self.extended_pid {
            Some(previous) => previous.wrapping_add(next.wrapping_sub(previous_picture_id)),
            None => next,
        });

        Ok(payload_index)
    }

    fn parse_layer_info(
        &mut self,
        reader: &mut dyn BitRead,
        mut payload_index: usize,
    ) -> Result<usize, PacketError> {
        payload_index = self.parse_layer_info_common(reader, payload_index)?;

        if self.f {
            Ok(payload_index)
        } else {
            self.parse_layer_info_non_flexible_mode(reader, payload_index)
        }
    }

    // Layer indices (flexible mode):
    //
    //      +-+-+-+-+-+-+-+-+
    // L:   |  T  |U|  S  |D|
    //      +-+-+-+-+-+-+-+-+
    //
    fn parse_layer_info_common(
        &mut self,
        reader: &mut dyn BitRead,
        mut payload_index: usize,
    ) -> Result<usize, PacketError> {
        if reader.remaining_bits() == 0 {
            return Err(PacketError::ErrShortPacket);
        }
        let b = reader.get_u8().ok_or(PacketError::ErrShortPacket)?;
        payload_index += 1;

        self.tid = b >> 5;
        self.u = b & 0x10 != 0;
        self.sid = (b >> 1) & 0x7;
        self.d = b & 0x01 != 0;

        if self.sid as usize >= MAX_SPATIAL_LAYERS {
            Err(PacketError::ErrTooManySpatialLayers)
        } else {
            Ok(payload_index)
        }
    }

    // Layer indices (non-flexible mode):
    //
    //      +-+-+-+-+-+-+-+-+
    // L:   |  T  |U|  S  |D|
    //      +-+-+-+-+-+-+-+-+
    //      |   tl0picidx   |
    //      +-+-+-+-+-+-+-+-+
    //
    fn parse_layer_info_non_flexible_mode(
        &mut self,
        reader: &mut dyn BitRead,
        mut payload_index: usize,
    ) -> Result<usize, PacketError> {
        if reader.remaining_bits() == 0 {
            return Err(PacketError::ErrShortPacket);
        }
        self.tl0picidx = reader.get_u8().ok_or(PacketError::ErrShortPacket)?;
        self.extended_tl0picidx = Some(extend_u8(self.extended_tl0picidx, self.tl0picidx));
        payload_index += 1;
        Ok(payload_index)
    }

    // Reference indices:
    //
    //      +-+-+-+-+-+-+-+-+                P=1,F=1: At least one reference index
    // P,F: | P_DIFF      |N|  up to 3 times          has to be specified.
    //      +-+-+-+-+-+-+-+-+                    N=1: An additional P_DIFF follows
    //                                                current P_DIFF.
    //
    fn parse_ref_indices(
        &mut self,
        reader: &mut dyn BitRead,
        mut payload_index: usize,
    ) -> Result<usize, PacketError> {
        let mut b = 1u8;
        let mut num_ref_pics = 0;
        while (b & 0x1) != 0 {
            if num_ref_pics == MAX_VP9REF_PICS {
                return Err(PacketError::ErrTooManyPDiff);
            }

            if reader.remaining_bits() == 0 {
                return Err(PacketError::ErrShortPacket);
            }
            b = reader.get_u8().ok_or(PacketError::ErrShortPacket)?;
            payload_index += 1;

            self.pdiff[num_ref_pics] = b >> 1;
            num_ref_pics += 1;
        }

        Ok(payload_index)
    }

    // Scalability structure (SS):
    //
    //      +-+-+-+-+-+-+-+-+
    // V:   | N_S |Y|G|-|-|-|
    //      +-+-+-+-+-+-+-+-+              -|
    // Y:   |     WIDTH     | (OPTIONAL)    .
    //      +               +               .
    //      |               | (OPTIONAL)    .
    //      +-+-+-+-+-+-+-+-+               . N_S + 1 times
    //      |     HEIGHT    | (OPTIONAL)    .
    //      +               +               .
    //      |               | (OPTIONAL)    .
    //      +-+-+-+-+-+-+-+-+              -|
    // G:   |      N_G      | (OPTIONAL)
    //      +-+-+-+-+-+-+-+-+                           -|
    // N_G: |  T  |U| R |-|-| (OPTIONAL)                 .
    //      +-+-+-+-+-+-+-+-+              -|            . N_G times
    //      |    P_DIFF     | (OPTIONAL)    . R times    .
    //      +-+-+-+-+-+-+-+-+              -|           -|
    //
    fn parse_ssdata(
        &mut self,
        reader: &mut dyn BitRead,
        mut payload_index: usize,
    ) -> Result<usize, PacketError> {
        if reader.remaining_bits() == 0 {
            return Err(PacketError::ErrShortPacket);
        }

        let b = reader.get_u8().ok_or(PacketError::ErrShortPacket)?;
        payload_index += 1;

        self.ns = b >> 5;
        self.y = b & 0x10 != 0;
        self.g = (b >> 1) & 0x7 != 0;

        let ns = (self.ns + 1) as usize;
        self.ng = 0;

        if ns > MAX_SPATIAL_LAYERS {
            return Err(PacketError::ErrVP9CorruptedPacket);
        }

        if self.y {
            if reader.remaining_bits() < 4 * ns {
                return Err(PacketError::ErrShortPacket);
            }

            for i in 0..ns {
                self.width[i] = Some(reader.get_u16().ok_or(PacketError::ErrShortPacket)?);
                self.height[i] = Some(reader.get_u16().ok_or(PacketError::ErrShortPacket)?);
            }
            payload_index += 4 * ns;
        }

        if self.g {
            if reader.remaining_bits() == 0 {
                return Err(PacketError::ErrShortPacket);
            }

            self.ng = reader.get_u8().ok_or(PacketError::ErrShortPacket)?;
            payload_index += 1;
        }

        for i in 0..self.ng as usize {
            if reader.remaining_bits() == 0 {
                return Err(PacketError::ErrShortPacket);
            }
            let b = reader.get_u8().ok_or(PacketError::ErrShortPacket)?;
            payload_index += 1;

            self.pgtid.push(b >> 5);
            self.pgu.push(b & 0x10 != 0);

            let r = ((b >> 2) & 0x3) as usize;
            if reader.remaining_bits() < r {
                return Err(PacketError::ErrShortPacket);
            }

            self.pgpdiff.push(vec![]);
            for _ in 0..r {
                let b = reader.get_u8().ok_or(PacketError::ErrShortPacket)?;
                payload_index += 1;

                self.pgpdiff[i].push(b);
            }
        }

        Ok(payload_index)
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    #[allow(clippy::erasing_op, clippy::identity_op)]
    fn test_vp9_packet_unmarshal() -> Result<(), PacketError> {
        let tests: Vec<(&str, &[u8], Vp9Depacketizer, &[u8], Option<PacketError>)> = vec![
            (
                "Empty",
                &[],
                Vp9Depacketizer::default(),
                &[],
                Some(PacketError::ErrShortPacket),
            ),
            (
                "NonFlexible",
                &[0x00, 0xAA],
                Vp9Depacketizer::default(),
                &[0xAA],
                None,
            ),
            (
                "NonFlexiblePictureID",
                &[0x80, 0x02, 0xAA],
                Vp9Depacketizer {
                    i: true,
                    picture_id: 0x02,
                    extended_pid: Some(0x02),
                    ..Default::default()
                },
                &[0xAA],
                None,
            ),
            (
                "NonFlexiblePictureIDExt",
                &[0x80, 0x81, 0xFF, 0xAA],
                Vp9Depacketizer {
                    i: true,
                    picture_id: 0x01FF,
                    extended_pid: Some(0x01FF),
                    ..Default::default()
                },
                &[0xAA],
                None,
            ),
            (
                "NonFlexiblePictureIDExt_ShortPacket0",
                &[0x80, 0x81],
                Vp9Depacketizer::default(),
                &[],
                Some(PacketError::ErrShortPacket),
            ),
            (
                "NonFlexiblePictureIDExt_ShortPacket1",
                &[0x80],
                Vp9Depacketizer::default(),
                &[],
                Some(PacketError::ErrShortPacket),
            ),
            (
                "NonFlexibleLayerIndicePictureID",
                &[0xA0, 0x02, 0x23, 0x01, 0xAA],
                Vp9Depacketizer {
                    i: true,
                    l: true,
                    picture_id: 0x02,
                    extended_pid: Some(0x02),
                    tid: 0x01,
                    sid: 0x01,
                    d: true,
                    tl0picidx: 0x01,
                    extended_tl0picidx: Some(0x01),
                    ..Default::default()
                },
                &[0xAA],
                None,
            ),
            (
                "FlexibleLayerIndicePictureID",
                &[0xB0, 0x02, 0x23, 0x01, 0xAA],
                Vp9Depacketizer {
                    f: true,
                    i: true,
                    l: true,
                    picture_id: 0x02,
                    extended_pid: Some(0x02),
                    tid: 0x01,
                    sid: 0x01,
                    d: true,
                    ..Default::default()
                },
                &[0x01, 0xAA],
                None,
            ),
            (
                "NonFlexibleLayerIndicePictureID_ShortPacket0",
                &[0xA0, 0x02, 0x23],
                Vp9Depacketizer::default(),
                &[],
                Some(PacketError::ErrShortPacket),
            ),
            (
                "NonFlexibleLayerIndicePictureID_ShortPacket1",
                &[0xA0, 0x02],
                Vp9Depacketizer::default(),
                &[],
                Some(PacketError::ErrShortPacket),
            ),
            (
                "FlexiblePictureIDRefIndex",
                &[0xD0, 0x02, 0x03, 0x04, 0xAA],
                Vp9Depacketizer {
                    i: true,
                    p: true,
                    f: true,
                    picture_id: 0x02,
                    extended_pid: Some(0x02),
                    pdiff: [0x01, 0x02, 0],
                    ..Default::default()
                },
                &[0xAA],
                None,
            ),
            (
                "FlexiblePictureIDRefIndex_TooManyPDiff",
                &[0xD0, 0x02, 0x03, 0x05, 0x07, 0x09, 0x10, 0xAA],
                Vp9Depacketizer::default(),
                &[],
                Some(PacketError::ErrTooManyPDiff),
            ),
            (
                "FlexiblePictureIDRefIndexNoPayload",
                &[0xD0, 0x02, 0x03, 0x04],
                Vp9Depacketizer {
                    i: true,
                    p: true,
                    f: true,
                    picture_id: 0x02,
                    extended_pid: Some(0x02),
                    pdiff: [0x01, 0x02, 0],
                    ..Default::default()
                },
                &[],
                None,
            ),
            (
                "FlexiblePictureIDRefIndex_ShortPacket0",
                &[0xD0, 0x02, 0x03],
                Vp9Depacketizer::default(),
                &[],
                Some(PacketError::ErrShortPacket),
            ),
            (
                "FlexiblePictureIDRefIndex_ShortPacket1",
                &[0xD0, 0x02],
                Vp9Depacketizer::default(),
                &[],
                Some(PacketError::ErrShortPacket),
            ),
            (
                "FlexiblePictureIDRefIndex_ShortPacket2",
                &[0xD0],
                Vp9Depacketizer::default(),
                &[],
                Some(PacketError::ErrShortPacket),
            ),
            (
                "ScalabilityStructureResolutionsNoPayload",
                &[
                    0x0A,
                    (1 << 5) | (1 << 4), // NS:1 Y:1 G:0
                    (640 >> 8) as u8,
                    (640 & 0xff) as u8,
                    (360 >> 8) as u8,
                    (360 & 0xff) as u8,
                    (1280 >> 8) as u8,
                    (1280 & 0xff) as u8,
                    (720 >> 8) as u8,
                    (720 & 0xff) as u8,
                ],
                Vp9Depacketizer {
                    b: true,
                    v: true,
                    ns: 1,
                    y: true,
                    g: false,
                    ng: 0,
                    width: {
                        let mut res = [None; MAX_SPATIAL_LAYERS];
                        res[0] = Some(640);
                        res[1] = Some(1280);

                        res
                    },
                    height: {
                        let mut res = [None; MAX_SPATIAL_LAYERS];
                        res[0] = Some(360);
                        res[1] = Some(720);

                        res
                    },
                    ..Default::default()
                },
                &[],
                None,
            ),
            (
                "ScalabilityStructureNoPayload",
                &[
                    0x0A,
                    (1 << 5) | (1 << 3), // NS:1 Y:0 G:1
                    2,
                    (1 << 4),            // T:0 U:1 R:0 -
                    (2 << 5) | (1 << 2), // T:2 U:0 R:1 -
                    33,
                ],
                Vp9Depacketizer {
                    b: true,
                    v: true,
                    ns: 1,
                    y: false,
                    g: true,
                    ng: 2,
                    pgtid: vec![0, 2],
                    pgu: vec![true, false],
                    pgpdiff: vec![vec![], vec![33]],
                    ..Default::default()
                },
                &[],
                None,
            ),
            (
                "ScalabilityStructureNoPictureGroupNoPayload",
                &[
                    0x0A,
                    1 << 3, // NS:0 Y:0 G:1
                    0,      // N_G=0, valid: single temporal layer / no fixed dependency info
                ],
                Vp9Depacketizer {
                    b: true,
                    v: true,
                    ns: 0,
                    y: false,
                    g: true,
                    ng: 0,
                    ..Default::default()
                },
                &[],
                None,
            ),
            (
                "ScalabilityStructureThreeLayers",
                &[
                    0x0A,
                    (2 << 5) | (1 << 4), // NS:2 Y:1 G:0
                    (480 >> 8) as u8,
                    (480 & 0xff) as u8,
                    (270 >> 8) as u8,
                    (270 & 0xff) as u8,
                    (960 >> 8) as u8,
                    (960 & 0xff) as u8,
                    (540 >> 8) as u8,
                    (540 & 0xff) as u8,
                    (1920 >> 8) as u8,
                    (1920 & 0xff) as u8,
                    (1080 >> 8) as u8,
                    (1080 & 0xff) as u8,
                ],
                Vp9Depacketizer {
                    b: true,
                    v: true,
                    ns: 2,
                    y: true,
                    g: false,
                    ng: 0,
                    width: [Some(480), Some(960), Some(1920)],
                    height: [Some(270), Some(540), Some(1080)],
                    ..Default::default()
                },
                &[],
                None,
            ),
            (
                "ParseRefIdx",
                &[
                    0xD8,                              /* I:1 P:1 L:0 F:1
                                                        * B:1 E:0 V:0 Z:0 */
                    (0x80 | ((17 >> 8) & 0x7F)) as u8, // Two byte pictureID.
                    17,                                // TL0PICIDX
                    (17 << 1) | 1,                     // P_DIFF N:1
                    (18 << 1) | 1,                     // P_DIFF N:1
                    (127 << 1) | 0,                    // P_DIFF N:0
                ],
                Vp9Depacketizer {
                    i: true,
                    p: true,
                    f: true,
                    b: true,
                    picture_id: 17,
                    extended_pid: Some(17),
                    pdiff: [17, 18, 127],
                    ..Default::default()
                },
                &[],
                None,
            ),
        ];

        for (name, b, pkt, expected, err) in tests {
            let mut p = Vp9Depacketizer::default();

            if let Some(expected) = err {
                let mut payload = Vec::new();
                let mut extra = CodecExtra::None;
                if let Err(actual) = p.depacketize(b, &mut payload, &mut extra) {
                    assert_eq!(
                        expected, actual,
                        "{name}: expected {expected}, but got {actual}"
                    );
                } else {
                    panic!("{name}: expected error, but got passed");
                }
            } else {
                let mut payload = Vec::new();
                let mut extra = CodecExtra::None;
                p.depacketize(b, &mut payload, &mut extra)?;
                assert_eq!(pkt, p, "{name}: expected {pkt:?}, but got {p:?}");
                assert_eq!(payload, expected);
            }
        }

        Ok(())
    }

    #[test]
    fn test_vp9_packetizer_payload() -> Result<(), PacketError> {
        let mut rands = vec![];
        for r0 in 8692..8702 {
            rands.push(vec![(r0 >> 8) as u8 | 0x80, (r0 & 0xFF) as u8]);
        }

        // Non-flexible mode (F=0) header layout:
        //   Byte 0: I|P|L|F|B|E|V|Z  (I=1, L=1 always set)
        //   Bytes 1-2: 15-bit PID (M=1)
        //   Byte 3: TID(3)|U(1)|SID(3)|D(1) = 0x10 (TID=0, U=1, SID=0, D=0)
        //   Byte 4: tl0picidx (wrapping u8, increments every frame)
        //   [Byte 5]: SS data on keyframe first packet only (N_S=0, Y=0, G=0)
        //
        // Flags byte (non-keyframe): I=1,P=1,L=1,F=0 = 0xE0 base
        //   + B=0x08, E=0x04
        //   B+E: 0xEC, B only: 0xE8, E only: 0xE4, neither: 0xE0
        //
        // Test payloads (0x01, 0x02, etc.) don't match VP9 bitstream keyframe
        // pattern (frame_marker != 0b10), so they are treated as inter-frames (P=1).
        // tl0picidx starts at 0 and increments by 1 per frame.
        let tests: Vec<(&str, Vec<Vec<u8>>, usize, Vec<Vec<u8>>)> = vec![
            ("NilPayload", vec![vec![]], 100, vec![]),
            ("SmallMTU", vec![vec![0x00, 0x00]], 1, vec![]),
            ("NegativeMTU", vec![vec![0x00, 0x00]], 0, vec![]),
            (
                // Inter-frame, single packet: B+E set, tl0picidx=1
                "OnePacket",
                vec![vec![0x01, 0x02]],
                10,
                vec![vec![0xEC, rands[0][0], rands[0][1], 0x10, 0x01, 0x01, 0x02]],
            ),
            (
                // Inter-frame, 2 packets: MTU=9 → max_frag=1, tl0picidx=1
                "TwoPackets",
                vec![vec![0x01, 0x02]],
                9,
                vec![
                    vec![0xE8, rands[0][0], rands[0][1], 0x10, 0x01, 0x01],
                    vec![0xE4, rands[0][0], rands[0][1], 0x10, 0x01, 0x02],
                ],
            ),
            (
                // Inter-frame, 3 packets: MTU=9 → max_frag=1, tl0picidx=1
                "ThreePackets",
                vec![vec![0x01, 0x02, 0x03]],
                9,
                vec![
                    vec![0xE8, rands[0][0], rands[0][1], 0x10, 0x01, 0x01],
                    vec![0xE0, rands[0][0], rands[0][1], 0x10, 0x01, 0x02],
                    vec![0xE4, rands[0][0], rands[0][1], 0x10, 0x01, 0x03],
                ],
            ),
            (
                // Two inter-frames: frame1 tl0picidx=1, frame2 tl0picidx=2
                "TwoFramesFourPackets",
                vec![vec![0x01, 0x02, 0x03], vec![0x04]],
                10,
                vec![
                    vec![0xE8, rands[0][0], rands[0][1], 0x10, 0x01, 0x01, 0x02],
                    vec![0xE4, rands[0][0], rands[0][1], 0x10, 0x01, 0x03],
                    vec![0xEC, rands[1][0], rands[1][1], 0x10, 0x02, 0x04],
                ],
            ),
        ];

        for (name, bs, mtu, expected) in tests {
            let mut pck = Vp9Packetizer {
                initial_picture_id: 8692,
                ..Default::default()
            };

            let mut actual = vec![];
            for b in &bs {
                actual.extend(pck.packetize(mtu, b)?);
            }
            assert_eq!(expected, actual, "{name}: Payloaded packet");
        }

        //"PictureIDOverflow"
        {
            let mut pck = Vp9Packetizer {
                initial_picture_id: 8692,
                ..Default::default()
            };
            let mut p_prev = Vp9Depacketizer::default();
            for i in 0..0x8000 {
                let res = pck.packetize(9, &[0x01])?;
                let mut p = Vp9Depacketizer::default();
                let mut payload = Vec::new();
                let mut extra = CodecExtra::None;
                p.depacketize(&res[0], &mut payload, &mut extra)?;

                if i > 0 {
                    if p_prev.picture_id == 0x7FFF {
                        assert_eq!(
                            p.picture_id, 0,
                            "Picture ID next to 0x7FFF must be 0, got {}",
                            p.picture_id
                        );
                    } else if p_prev.picture_id + 1 != p.picture_id {
                        panic!(
                            "Picture ID next must be incremented by 1: {} -> {}",
                            p_prev.picture_id, p.picture_id,
                        );
                    }
                }

                p_prev = p;
            }
        }

        Ok(())
    }

    #[test]
    fn test_vp9_packetizer_keyframe() -> Result<(), PacketError> {
        let mut pck = Vp9Packetizer {
            initial_picture_id: 100,
            ..Default::default()
        };

        // VP9 profile 0 keyframe bitstream: 0x82 = 0b10_00_0_0_10
        // frame_marker=10, profile=00, show_existing=0, frame_type=0 (KEY)
        let keyframe = vec![0x82, 0xAA, 0xBB];
        let packets = pck.packetize(20, &keyframe)?;

        assert_eq!(packets.len(), 1);
        let pkt = &packets[0];
        // Flags: I=1,P=0,L=1,F=0,B=1,E=1,V=1,Z=0 = 0xAE
        assert_eq!(pkt[0], 0xAE, "Keyframe flags should be 0xAE");
        // PID
        assert_eq!(pkt[1], (100 >> 8) as u8 | 0x80);
        assert_eq!(pkt[2], 100);
        // L byte: TID=0, U=1, SID=0, D=0
        assert_eq!(pkt[3], 0x10);
        // tl0picidx = 1 (first frame)
        assert_eq!(pkt[4], 1);
        // SS: N_S=0, Y=0, G=0 (no GOF) → 0x00 (1 byte)
        assert_eq!(pkt[5], 0x00);
        // Payload starts right after the 1-byte SS
        assert_eq!(&pkt[6..], &keyframe[..]);

        // Parse-side: the emitted SS decodes as a single-layer, no-GOF structure (N_S=0, Y/G false).
        let mut dep = Vp9Depacketizer::default();
        let mut out = Vec::new();
        let mut extra = CodecExtra::None;
        dep.depacketize(pkt, &mut out, &mut extra)?;
        assert!(dep.v && dep.ns == 0 && !dep.y && !dep.g && dep.ng == 0);
        assert_eq!(&out[..], &keyframe[..]);

        // Now an inter-frame: 0x84 = 0b10_00_0_1_00 (frame_type=1)
        let inter = vec![0x84, 0xCC];
        let packets = pck.packetize(20, &inter)?;
        assert_eq!(packets.len(), 1);
        let pkt = &packets[0];
        // Flags: I=1,P=1,L=1,F=0,B=1,E=1,V=0,Z=0 = 0xEC
        assert_eq!(pkt[0], 0xEC, "Inter-frame flags should be 0xEC");
        // tl0picidx = 2 (second frame, both TID=0 so both increment)
        assert_eq!(pkt[4], 2);
        // No SS data — payload starts at byte 5
        assert_eq!(&pkt[5..], &inter[..]);

        Ok(())
    }

    #[test]
    fn test_vp9_packetizer_fragmented_keyframe() -> Result<(), PacketError> {
        let mut pck = Vp9Packetizer {
            initial_picture_id: 50,
            ..Default::default()
        };

        // VP9 profile 0 keyframe: 0x82 = frame_marker=10, profile=00, show_existing=0, frame_type=0
        // 10 bytes of payload, MTU=12 → max_frag = 12 - 8 = 4 (conservative reserve)
        // First packet: 6-byte header (with 1-byte G=0 SS) + 4 bytes payload = 10 ≤ MTU
        // Second packet: 5-byte header + 4 bytes payload = 9 ≤ MTU
        // Third packet: 5-byte header + 2 bytes payload = 7 ≤ MTU
        let keyframe = vec![0x82, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09];
        let packets = pck.packetize(12, &keyframe)?;

        assert_eq!(
            packets.len(),
            3,
            "10-byte keyframe at MTU=12 should produce 3 packets"
        );

        // Packet 1: B=1, E=0, V=1 (keyframe first)
        // Flags: I=1,P=0,L=1,F=0,B=1,E=0,V=1,Z=0 = 0xAA
        assert_eq!(packets[0][0], 0xAA);
        assert_eq!(packets[0].len(), 10); // 6-byte header + 4-byte payload
        assert_eq!(packets[0][3], 0x10); // L byte
        assert_eq!(packets[0][4], 1); // tl0picidx
        assert_eq!(packets[0][5], 0x00); // SS byte: N_S=0, Y=0, G=0 (no GOF)
        assert_eq!(&packets[0][6..], &keyframe[0..4]); // payload

        // Packet 2: B=0, E=0, V=0 (middle)
        // Flags: I=1,P=0,L=1,F=0,B=0,E=0,V=0,Z=0 = 0xA0
        assert_eq!(packets[1][0], 0xA0);
        assert_eq!(packets[1].len(), 9); // 5-byte header + 4-byte payload
        assert_eq!(packets[1][4], 1); // same tl0picidx
        assert_eq!(&packets[1][5..], &keyframe[4..8]);

        // Packet 3: B=0, E=1, V=0 (last)
        // Flags: I=1,P=0,L=1,F=0,B=0,E=1,V=0,Z=0 = 0xA4
        assert_eq!(packets[2][0], 0xA4);
        assert_eq!(packets[2].len(), 7); // 5-byte header + 2-byte payload
        assert_eq!(&packets[2][5..], &keyframe[8..10]);

        // Verify none exceed MTU
        for (i, pkt) in packets.iter().enumerate() {
            assert!(
                pkt.len() <= 12,
                "Packet {i} exceeds MTU: {} > 12",
                pkt.len()
            );
        }

        // Verify depacketizer round-trip: all 3 packets produce original payload
        let mut full_payload = Vec::new();
        let mut extra = CodecExtra::None;
        for pkt in &packets {
            let mut depkt = Vp9Depacketizer::default();
            depkt.depacketize(pkt, &mut full_payload, &mut extra)?;
        }
        assert_eq!(full_payload, keyframe);

        Ok(())
    }

    #[test]
    fn test_vp9_tl0picidx_wrapping() -> Result<(), PacketError> {
        let mut pck = Vp9Packetizer {
            initial_picture_id: 0,
            tl0picidx: 254, // Start near wrap point
            ..Default::default()
        };

        // Frame 1: tl0picidx wraps 254 → 255
        let packets = pck.packetize(20, &[0x01])?;
        assert_eq!(packets[0][4], 255);

        // Frame 2: tl0picidx wraps 255 → 0
        let packets = pck.packetize(20, &[0x01])?;
        assert_eq!(packets[0][4], 0);

        // Frame 3: 0 → 1
        let packets = pck.packetize(20, &[0x01])?;
        assert_eq!(packets[0][4], 1);

        Ok(())
    }

    fn depacketize_one(depack: &mut Vp9Depacketizer, packet: &[u8]) -> Vp9CodecExtra {
        let mut payload = Vec::new();
        let mut extra = CodecExtra::None;
        depack
            .depacketize(packet, &mut payload, &mut extra)
            .unwrap();
        let CodecExtra::Vp9(extra) = extra else {
            panic!("expected VP9 codec extra");
        };
        extra
    }

    #[test]
    fn depacketizer_extends_picture_id_and_tl0picidx() {
        let mut depack = Vp9Depacketizer::default();

        for (pid, tl0, expected_pid, expected_tl0) in [
            (0x7FFE_u16, 254_u8, 0x7FFE_u64, 254_u64),
            (0x7FFF, 255, 0x7FFF, 255),
            (0, 0, 0x8000, 256),
            (1, 1, 0x8001, 257),
        ] {
            // I=1 L=1 B=1 E=1, 15-bit picture id, TID=0, tl0picidx
            let packet = [0xAC, 0x80 | (pid >> 8) as u8, pid as u8, 0x00, tl0, 0xAA];
            let extra = depacketize_one(&mut depack, &packet);
            assert_eq!(extra.picture_id, Some(expected_pid), "pid {pid}");
            assert_eq!(extra.tl0_picture_id, Some(expected_tl0), "tl0 {tl0}");
        }
    }

    #[test]
    fn depacketizer_extends_7_bit_picture_id() {
        let mut depack = Vp9Depacketizer::default();

        for (pid, expected) in [(126_u8, 126_u64), (127, 127), (0, 128), (1, 129)] {
            // I=1 B=1 E=1, 7-bit picture id
            let extra = depacketize_one(&mut depack, &[0x8C, pid, 0xAA]);
            assert_eq!(extra.picture_id, Some(expected), "pid {pid}");
        }
    }

    #[test]
    fn depacketizer_reports_missing_picture_id() {
        let mut depack = Vp9Depacketizer::default();

        let extra = depacketize_one(&mut depack, &[0xAC, 0x92, 0x34, 0x00, 7, 0xAA]);
        assert_eq!(extra.picture_id, Some(0x1234));

        // I=0 L=1 B=1 E=1
        let extra = depacketize_one(&mut depack, &[0x2C, 0x00, 8, 0xAA]);
        assert_eq!(extra.picture_id, None);
        assert_eq!(extra.tl0_picture_id, Some(8));

        // Picture id on the first packet of the frame only
        let mut payload = Vec::new();
        let mut extra = CodecExtra::None;
        depack
            .depacketize(&[0xA8, 0x92, 0x35, 0x00, 9, 0xAA], &mut payload, &mut extra)
            .unwrap();
        depack
            .depacketize(&[0x24, 0x00, 9, 0xBB], &mut payload, &mut extra)
            .unwrap();
        let CodecExtra::Vp9(extra) = extra else {
            panic!("expected VP9 codec extra");
        };
        assert_eq!(extra.picture_id, Some(0x1235));
    }

    fn assert_every_frame_emitted(frames: impl IntoIterator<Item = Vec<Vec<u8>>>) {
        use crate::packet::{CodecDepacketizer, DepacketizingBuffer, RtpMeta};
        use crate::rtp_::{MediaTime, RtpHeader};
        use std::time::Instant;

        let mut buf =
            DepacketizingBuffer::new(CodecDepacketizer::Vp9(Vp9Depacketizer::default()), 3);
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
    fn depacketizing_buffer_emits_every_frame_across_wraps() {
        let mut pck = Vp9Packetizer {
            initial_picture_id: 0x7FFF - 100,
            tl0picidx: 200,
            ..Default::default()
        };
        let frames: Vec<_> = (0..300)
            .map(|frame| {
                let first_byte = if frame == 0 { 0x80 } else { 0x84 };
                pck.packetize(10, &[first_byte, 1, 2, 3]).unwrap()
            })
            .collect();

        assert_every_frame_emitted(frames);
    }

    #[test]
    fn depacketizing_buffer_emits_every_frame_with_7_bit_picture_id() {
        // I=1 L=1 B=1 E=1 (P=1 after the first frame), 7-bit picture id, TID=0, tl0picidx
        let frames = (0..600_u64).map(|frame| {
            let flags = if frame == 0 { 0xAC } else { 0xEC };
            vec![vec![flags, (frame & 0x7F) as u8, 0x00, frame as u8, 0x84]]
        });

        assert_every_frame_emitted(frames);
    }

    #[test]
    fn depacketizing_buffer_emits_every_frame_without_picture_id() {
        // I=0 L=1 B=1 E=1 (P=1 after the first frame), TID=0, tl0picidx
        let frames = (0..300_u64).map(|frame| {
            let flags = if frame == 0 { 0x2C } else { 0x6C };
            vec![vec![flags, 0x00, frame as u8, 0x84]]
        });

        assert_every_frame_emitted(frames);
    }

    #[test]
    fn depacketizer_preserves_picture_id_epoch_when_width_grows() {
        let mut depack = Vp9Depacketizer::default();
        for (pid, expected) in [(126_u8, 126_u64), (127, 127), (0, 128), (1, 129)] {
            let extra = depacketize_one(&mut depack, &[0x8C, pid, 0xAA]);
            assert_eq!(extra.picture_id, Some(expected));
        }
        // RFC 9628 section 4.2: transitioning to 15 bits zero-extends the wire ID.
        let extra = depacketize_one(&mut depack, &[0x8C, 0x80, 2, 0xAA]);
        assert_eq!(extra.picture_id, Some(130));
    }

    #[test]
    fn depacketizing_buffer_emits_frames_when_picture_id_width_grows() {
        let frames = [
            vec![vec![0xAC, 126, 0, 10, 0x80]],
            vec![vec![0xEC, 127, 0, 11, 0x84]],
            // A keyframe at the short-ID wrap lets the pre-PR code resynchronize.
            vec![vec![0xAC, 0, 0, 12, 0x80]],
            vec![vec![0xEC, 1, 0, 13, 0x84]],
            // The following picture has wire ID 0x0002 and M=1.
            vec![vec![0xEC, 0x80, 2, 0, 14, 0x84]],
            vec![vec![0xEC, 0x80, 3, 0, 15, 0x84]],
        ];
        assert_every_frame_emitted(frames);
    }

    #[test]
    fn depacketizer_preserves_wide_picture_id_after_loss_at_width_growth() {
        let mut depack = Vp9Depacketizer::default();
        assert_eq!(
            depacketize_one(&mut depack, &[0x8C, 0, 0xAA]).picture_id,
            Some(0)
        );
        // The sender widens at PID 1; pictures 1..99 are lost in transit.
        let extra = depacketize_one(&mut depack, &[0x8C, 0x80, 100, 0xAA]);
        assert_eq!(extra.picture_id, Some(100));
    }

    #[test]
    fn depacketizing_buffer_recovers_after_loss_at_picture_id_width_growth() {
        // A middlebox drops enhancement pictures 1..99 and rewrites RTP sequence numbers.
        // Picture 100 is a keyframe, so all following pictures are decodable.
        let frames =
            std::iter::once(vec![vec![0xAC, 0, 0, 0, 0x80]]).chain((100..=130_u8).map(|pid| {
                let (flags, payload) = if pid == 100 {
                    (0xAC, 0x80)
                } else {
                    (0xEC, 0x84)
                };
                vec![vec![flags, 0x80, pid, 0, pid - 99, payload]]
            }));
        assert_every_frame_emitted(frames);
    }

    #[test]
    fn test_vp9_packetizer_flexible_mode() -> Result<(), PacketError> {
        let mut rands = vec![];
        for r0 in 8692..8702 {
            rands.push(vec![(r0 >> 8) as u8 | 0x80, (r0 & 0xFF) as u8]);
        }

        // With VP9_FLEXIBLE_HEADER_SIZE=3, min productive MTU is 4.
        // Flexible mode: 3 bytes header (flags + 15-bit PID)
        //
        // Flags byte: I|P|L|F|B|E|V|Z
        //   I=1(0x80), F=1(0x10), B=first(0x08), E=last(0x04)
        //   B+E: 0x80|0x10|0x08|0x04 = 0x9C
        //   B:   0x80|0x10|0x08       = 0x98
        //   E:   0x80|0x10|0x04       = 0x94
        //   none:0x80|0x10            = 0x90
        let tests: Vec<(&str, Vec<Vec<u8>>, usize, Vec<Vec<u8>>)> = vec![
            ("NilPayload", vec![vec![]], 100, vec![]),
            ("SmallMTU", vec![vec![0x00, 0x00]], 1, vec![]),
            ("NegativeMTU", vec![vec![0x00, 0x00]], 0, vec![]),
            (
                "OnePacket",
                vec![vec![0x01, 0x02]],
                10,
                vec![vec![0x9C, rands[0][0], rands[0][1], 0x01, 0x02]],
            ),
            (
                // MTU=4 → max_frag=1 → 2 packets
                "TwoPackets",
                vec![vec![0x01, 0x02]],
                4,
                vec![
                    vec![0x98, rands[0][0], rands[0][1], 0x01],
                    vec![0x94, rands[0][0], rands[0][1], 0x02],
                ],
            ),
            (
                // MTU=4 → max_frag=1 → 3 packets
                "ThreePackets",
                vec![vec![0x01, 0x02, 0x03]],
                4,
                vec![
                    vec![0x98, rands[0][0], rands[0][1], 0x01],
                    vec![0x90, rands[0][0], rands[0][1], 0x02],
                    vec![0x94, rands[0][0], rands[0][1], 0x03],
                ],
            ),
            (
                // MTU=5 → max_frag=2 → frame1: 2 pkts, frame2: 1 pkt
                "TwoFramesFourPackets",
                vec![vec![0x01, 0x02, 0x03], vec![0x04]],
                5,
                vec![
                    vec![0x98, rands[0][0], rands[0][1], 0x01, 0x02],
                    vec![0x94, rands[0][0], rands[0][1], 0x03],
                    vec![0x9C, rands[1][0], rands[1][1], 0x04],
                ],
            ),
        ];

        for (name, bs, mtu, expected) in tests {
            let mut pck = Vp9Packetizer {
                initial_picture_id: 8692,
                mode: Vp9PacketizerMode::Flexible,
                ..Default::default()
            };

            let mut actual = vec![];
            for b in &bs {
                actual.extend(pck.packetize(mtu, b)?);
            }
            assert_eq!(expected, actual, "{name}: Payloaded packet");
        }

        Ok(())
    }

    #[test]
    fn test_vp9_packetizer_with_mode() {
        let pck = Vp9Packetizer::with_mode(Vp9PacketizerMode::Flexible);
        assert_eq!(pck.mode, Vp9PacketizerMode::Flexible);

        let pck = Vp9Packetizer::with_mode(Vp9PacketizerMode::NonFlexible);
        assert_eq!(pck.mode, Vp9PacketizerMode::NonFlexible);

        let pck = Vp9Packetizer::default();
        assert_eq!(pck.mode, Vp9PacketizerMode::NonFlexible);
    }

    #[test]
    fn test_detect_vp9_keyframe_bitstream() {
        // Empty payload
        assert!(!detect_vp9_keyframe_bitstream(&[]));

        // Invalid frame_marker (not 0b10)
        assert!(!detect_vp9_keyframe_bitstream(&[0x00])); // frame_marker=00
        assert!(!detect_vp9_keyframe_bitstream(&[0xC0])); // frame_marker=11
        assert!(!detect_vp9_keyframe_bitstream(&[0x40])); // frame_marker=01

        // Profile 0 keyframe: 10_00_0_0_xx = 0x80..0x83
        assert!(detect_vp9_keyframe_bitstream(&[0x80]));
        assert!(detect_vp9_keyframe_bitstream(&[0x82]));

        // Profile 0 inter-frame: 10_00_0_1_xx = 0x84..0x87
        assert!(!detect_vp9_keyframe_bitstream(&[0x84]));
        assert!(!detect_vp9_keyframe_bitstream(&[0x86]));

        // Profile 0 show_existing_frame=1: 10_00_1_x_xx = 0x88..0x8F
        assert!(!detect_vp9_keyframe_bitstream(&[0x88]));
        assert!(!detect_vp9_keyframe_bitstream(&[0x8C]));

        // Profile 1 keyframe: 10_10_0_0_xx = 0xA0..0xA3
        assert!(detect_vp9_keyframe_bitstream(&[0xA0]));

        // Profile 1 inter-frame: 10_10_0_1_xx = 0xA4..0xA7
        assert!(!detect_vp9_keyframe_bitstream(&[0xA4]));

        // Profile 2 keyframe: 10_01_R_0_0_x
        // 0x90 = 10_01_0_0_00 → profile_high=1, show_existing=0, frame_type=0
        assert!(detect_vp9_keyframe_bitstream(&[0x90]));

        // Profile 2 inter-frame: 10_01_0_0_1_0 = 0x92
        assert!(!detect_vp9_keyframe_bitstream(&[0x92]));
    }

    #[test]
    fn test_vp9_partition_head_checker_is_partition_head() -> Result<(), PacketError> {
        let vp9 = Vp9Depacketizer::default();

        //"SmallPacket"
        assert!(
            !vp9.is_partition_head(&[]),
            "Small packet should not be the head of a new partition"
        );

        //"NormalPacket"
        assert!(
            vp9.is_partition_head(&[0x18, 0x00, 0x00]),
            "VP9 RTP packet with B flag should be head of a new partition"
        );
        assert!(
            !vp9.is_partition_head(&[0x10, 0x00, 0x00]),
            "VP9 RTP packet without B flag should not be head of a new partition"
        );

        Ok(())
    }

    #[test]
    fn test_detect_vp9_keyframe() {
        // Empty payload
        assert!(!detect_vp9_keyframe(&[]));

        // VP9 RTP descriptor byte 0: I|P|L|F|B|E|V|Z
        // P=0 (bit 6 clear) → keyframe
        // P=1 (bit 6 set) → inter-frame

        // Keyframes (P=0)
        assert!(detect_vp9_keyframe(&[0x80])); // I=1
        assert!(detect_vp9_keyframe(&[0xA0])); // I=1, L=1
        assert!(detect_vp9_keyframe(&[0xAE])); // I=1, L=1, B=1, E=1, V=1
        assert!(detect_vp9_keyframe(&[0x00])); // all flags clear

        // Inter-frames (P=1)
        assert!(!detect_vp9_keyframe(&[0xE8])); // I=1, P=1, L=1, B=1
        assert!(!detect_vp9_keyframe(&[0xEC])); // I=1, P=1, L=1, B=1, E=1
        assert!(!detect_vp9_keyframe(&[0x40])); // P=1 only
        assert!(!detect_vp9_keyframe(&[0xD5])); // I=1, P=1, F=1, E=1, Z=1
    }

    #[test]
    fn packetize_respects_mtu() -> Result<(), PacketError> {
        // VP9 profile 0 keyframe header followed by payload bytes.
        let mut payload = vec![0x82u8];
        payload.extend(std::iter::repeat_n(0xABu8, 2000));
        for &mtu in &[100usize, 300, 600, 1200] {
            let mut pck = Vp9Packetizer {
                initial_picture_id: 100,
                ..Default::default()
            };
            let pkts = pck.packetize(mtu, &payload)?;
            assert!(!pkts.is_empty(), "VP9 produced no packets at mtu {mtu}");
            for (i, pkt) in pkts.iter().enumerate() {
                assert!(
                    pkt.len() <= mtu,
                    "VP9 packet {i} size {} > mtu {mtu}",
                    pkt.len()
                );
            }
        }
        Ok(())
    }
}
