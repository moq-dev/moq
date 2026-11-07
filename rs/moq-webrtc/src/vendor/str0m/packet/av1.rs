use super::{BitRead, CodecExtra, Depacketizer, PacketError, Packetizer, encode_leb_u63};

const OBU_EXTENSION_PRESENT_MASK: u8 = 0b0000_0100;
const OBU_SIZE_PRESENT_MASK: u8 = 0b0000_0010;
const OBU_TYPE_MASK: u8 = 0b0111_1000;
const AGGREGATION_HEADER_SIZE: usize = 1;
const MAX_NUM_OBUS_TO_OMTI_SIZE: usize = 3;

/// Detect whether an AV1 RTP payload contains a keyframe.
///
/// Checks the N bit (new coded video sequence) in the AV1 aggregation header.
/// N=1 indicates the first packet of a keyframe (random access point).
///
/// AV1 aggregation header layout: `Z|Y|W W|N|reserved`
/// - N (bit 3): 1 = new coded video sequence starts
pub fn detect_av1_keyframe(payload: &[u8]) -> bool {
    if payload.is_empty() {
        return false;
    }
    // N bit is bit 3 of the aggregation header
    payload[0] & 0x08 != 0
}

/// AV1 information describing the depacketized / packetized data
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Av1CodecExtra {
    /// Flag which indicates that within [`MediaData`], there is an individual frame
    /// containing complete and independent visual information. This frame serves
    /// as a reference point for other frames in the video sequence.
    ///
    /// [`MediaData`]: crate::media::MediaData
    pub is_keyframe: bool,
}

/// AV1 packetizer
#[derive(Default, Debug)]
pub struct Av1Packetizer {
    packets: Vec<Packet>,
    obus: Vec<Obu>,
}

impl Av1Packetizer {
    fn emit(&mut self, mtu: usize, payloads: &mut Vec<Vec<u8>>) {
        if self.obus.is_empty() {
            return;
        }

        // Clear previous frame data
        self.packets.clear();

        // Reserve 1 byte for aggregation header
        let max_payload_size = mtu - AGGREGATION_HEADER_SIZE;
        let mut remaining_packet_size = max_payload_size;

        // Push as many obus as possible into each packet
        let mut current_packet = Packet::new(0);
        for (obu_idx, obu) in self.obus.iter().enumerate() {
            let mut previous_obu_extra_size = self.extra_size_for_previous_obu(&current_packet);
            let min_required_size = if current_packet.num_obu_elements >= MAX_NUM_OBUS_TO_OMTI_SIZE
            {
                2
            } else {
                1
            };

            // Check if new packet is needed
            if remaining_packet_size < previous_obu_extra_size + min_required_size {
                self.packets.push(current_packet);
                current_packet = Packet::new(obu_idx);
                remaining_packet_size = max_payload_size;
                previous_obu_extra_size = 0;
            }

            current_packet.packet_size += previous_obu_extra_size;
            current_packet.num_obu_elements += 1;
            remaining_packet_size -= previous_obu_extra_size;

            let must_write_obu_element_size =
                current_packet.num_obu_elements > MAX_NUM_OBUS_TO_OMTI_SIZE;
            let mut required_bytes = obu.size;
            if must_write_obu_element_size {
                required_bytes += leb_128_size(obu.size);
            }

            if required_bytes <= remaining_packet_size {
                current_packet.last_obu_size = obu.size;
                current_packet.packet_size += required_bytes;
                remaining_packet_size -= required_bytes;
                continue;
            }

            // Fragment the obu
            let max_first_fragment_size = match must_write_obu_element_size {
                true => self.max_fragment_size(remaining_packet_size),
                false => remaining_packet_size,
            };

            if max_first_fragment_size == 0 {
                current_packet.num_obu_elements -= 1;
                current_packet.packet_size -= previous_obu_extra_size;
            } else {
                current_packet.packet_size += max_first_fragment_size;
                if must_write_obu_element_size {
                    current_packet.packet_size += leb_128_size(max_first_fragment_size);
                }
                current_packet.last_obu_size = max_first_fragment_size;
            }

            // Middle fragments
            let mut obu_offset = max_first_fragment_size;
            while obu_offset + max_payload_size < obu.size {
                self.packets.push(current_packet);
                current_packet = Packet::new(obu_idx);
                current_packet.num_obu_elements = 1;
                current_packet.first_obu_offset = obu_offset;
                current_packet.last_obu_size = max_payload_size;
                current_packet.packet_size = max_payload_size;

                obu_offset += max_payload_size
            }

            // Last fragment
            let last_fragment_size = obu.size - obu_offset;
            self.packets.push(current_packet);
            current_packet = Packet::new(obu_idx);
            current_packet.num_obu_elements = 1;
            current_packet.first_obu_offset = obu_offset;
            current_packet.last_obu_size = last_fragment_size;
            current_packet.packet_size = last_fragment_size;

            remaining_packet_size = max_payload_size - last_fragment_size;
        }

        self.packets.push(current_packet);
        self.write_rtp_payloads(payloads)
    }

    fn write_rtp_payloads(&self, payloads: &mut Vec<Vec<u8>>) {
        for (i, packet) in self.packets.iter().enumerate() {
            let is_first_packet = i == 0;
            let mut rtp_payload: Vec<u8> = vec![0u8; AGGREGATION_HEADER_SIZE + packet.packet_size];
            let mut pos = 0;

            let header = self.aggregation_header(packet, &self.obus, is_first_packet);
            rtp_payload[pos] = header;
            pos += 1;

            let mut obu_offset = packet.first_obu_offset;
            // Write all obu elements except the last one
            for obu_idx in 0..(packet.num_obu_elements - 1) {
                let obu = &self.obus[packet.first_obu + obu_idx];
                let obu_fragment_size = obu.size - obu_offset;
                pos += encode_leb_u63(obu_fragment_size as u64, &mut rtp_payload[pos..]);

                if obu_offset == 0 {
                    rtp_payload[pos] = obu.header & !OBU_SIZE_PRESENT_MASK;
                    pos += 1;
                }

                if obu_offset <= 1 && obu.has_extension() {
                    rtp_payload[pos] = obu.ext_header;
                    pos += 1;
                }

                let payload_offset =
                    obu_offset.saturating_sub(if obu.has_extension() { 2 } else { 1 });
                let payload_size = obu.payload.len() - payload_offset;

                if !obu.payload.is_empty() && payload_size > 0 {
                    rtp_payload[pos..pos + payload_size].copy_from_slice(
                        &obu.payload[payload_offset..payload_offset + payload_size],
                    );
                }

                pos += payload_size;
                obu_offset = 0;
            }

            let last_obu = &self.obus[packet.first_obu + packet.num_obu_elements - 1];
            let mut obu_fragment_size = packet.last_obu_size;
            if packet.num_obu_elements > MAX_NUM_OBUS_TO_OMTI_SIZE {
                pos += encode_leb_u63(obu_fragment_size as u64, &mut rtp_payload[pos..]);
            }

            if obu_offset == 0 && obu_fragment_size > 0 {
                rtp_payload[pos] = last_obu.header & !OBU_SIZE_PRESENT_MASK;
                pos += 1;
                obu_fragment_size -= 1;
            }

            if obu_offset <= 1 && last_obu.has_extension() && obu_fragment_size > 0 {
                rtp_payload[pos] = last_obu.ext_header;
                pos += 1;
                obu_fragment_size -= 1;
            }

            let payload_offset =
                obu_offset.saturating_sub(if last_obu.has_extension() { 2 } else { 1 });
            rtp_payload[pos..pos + obu_fragment_size].copy_from_slice(
                &last_obu.payload[payload_offset..payload_offset + obu_fragment_size],
            );

            payloads.push(rtp_payload);
        }
    }

    fn aggregation_header(&self, packet: &Packet, obus: &[Obu], is_first_packet: bool) -> u8 {
        let mut agg_header = 0;

        // set Z flag: the first obu element is the continuation of the previous one
        if packet.first_obu_offset > 0 {
            agg_header |= 1 << 7;
        }

        // set Y flag: the last obu element is continued in the next packet
        let last_obu_offset = if packet.num_obu_elements == 1 {
            packet.first_obu_offset
        } else {
            0
        };
        let last_obu_is_fragment = (last_obu_offset + packet.last_obu_size)
            < obus[packet.first_obu + packet.num_obu_elements - 1].size;
        if last_obu_is_fragment {
            agg_header |= 1 << 6;
        }

        // set W field: small number of obu elements in the packet
        if packet.num_obu_elements <= MAX_NUM_OBUS_TO_OMTI_SIZE {
            agg_header |= packet.num_obu_elements << 4;
        }

        // set N flag:
        if let Some(ObuType::SequenceHeader) = obus[0].obu_type() {
            if is_first_packet {
                agg_header |= 1 << 3;
            }
        }

        agg_header as u8
    }

    /// Adding new OBU to the last packet would mean that the previous one is
    /// no longer the final OBU element in that packet. Any OBU that is not the
    /// last element must be prefixed with its length. `extra_size_for_previous_obu`
    /// computes the number of bytes required to encode that length.
    fn extra_size_for_previous_obu(&self, packet: &Packet) -> usize {
        if packet.packet_size == 0 {
            return 0;
        }

        if packet.num_obu_elements > MAX_NUM_OBUS_TO_OMTI_SIZE {
            return 0;
        }

        leb_128_size(packet.last_obu_size)
    }

    // Given the number of free bytes remaining in a packet, the function returns the largest
    // OBU fragment size that will fit into the packet
    // That is, FragmentSize + Leb128Size(FragmentSize) must not exceed remaining_bytes
    fn max_fragment_size(&self, size: usize) -> usize {
        if size <= 1 {
            return 0;
        }

        let mut idx = 1;
        loop {
            if size < (1 << 7 * idx) + 1 {
                return size - idx;
            }
            idx += 1;
        }
    }
}

impl Packetizer for Av1Packetizer {
    fn packetize(&mut self, mtu: usize, payload: &[u8]) -> Result<Vec<Vec<u8>>, PacketError> {
        if payload.is_empty() || mtu <= AGGREGATION_HEADER_SIZE {
            return Ok(vec![]);
        }

        let mut payloads = vec![];
        parse_obus(payload, &mut self.obus)?;
        self.emit(mtu, &mut payloads);

        Ok(payloads)
    }

    fn is_marker(&mut self, _data: &[u8], _previous: Option<&[u8]>, last: bool) -> bool {
        last
    }
}

/// AV1 Depacketizer
#[derive(Default, Debug)]
pub struct Av1Depacketizer {
    /// current obu payload
    obu_buffer: Vec<u8>,

    /// reusable buffer for parsed OBUs
    parsed_obus: Vec<Obu>,

    /// length of current obu
    obu_length: usize,

    /// aggregation header Z bit
    z: bool,

    /// aggregation header Y bit
    y: bool,

    /// aggregation header N bit
    n: bool,

    /// Number of obus in packet
    obu_count: u8,
}

impl Av1Depacketizer {
    fn parse_aggregation_header(&mut self, agg_header: u8) {
        // TODO: store these values as mask
        self.z = agg_header & (1 << 7) != 0;
        self.y = agg_header & (1 << 6) != 0;
        self.obu_count = (agg_header & 0b0011_0000) >> 4;
        self.n = agg_header & (1 << 3) != 0;
    }
}

impl Depacketizer for Av1Depacketizer {
    fn out_size_hint(&self, packets_size: usize) -> Option<usize> {
        Some(packets_size)
    }

    fn depacketize(
        &mut self,
        packet: &[u8],
        out: &mut Vec<u8>,
        codec_extra: &mut super::CodecExtra,
    ) -> Result<(), PacketError> {
        if packet.is_empty() {
            return Err(PacketError::ErrShortPacket);
        }

        let mut reader = (packet, 0);

        self.parse_aggregation_header(packet[0]);
        reader.skip_bytes(1);

        // if packet does not start with a continuation of an obu fragment
        // from the previous packet new obu starts
        if !self.z {
            self.obu_length = 0;
            self.obu_buffer.clear();
        }

        // set the key frame flag if N bit is set and clear the obu buffer
        // as it cannot start with a fragment of the previous packet
        let mut is_keyframe = matches!(
            *codec_extra,
            CodecExtra::Av1(Av1CodecExtra { is_keyframe: true })
        );
        if self.n {
            is_keyframe = true;
            self.obu_length = 0;
            self.obu_buffer.clear();
        }
        *codec_extra = CodecExtra::Av1(Av1CodecExtra { is_keyframe });

        let mut obu_idx: usize = 0;
        while reader.remaining_bits() > 0 {
            let is_first_obu = obu_idx == 0;
            let mut is_last_obu = self.obu_count != 0 && obu_idx == (self.obu_count as usize - 1);

            // Read the length of obu
            let fragment_obu_length = if self.obu_count == 0 || !is_last_obu {
                let len = reader
                    .get_leb128()
                    .ok_or(PacketError::ErrAv1CorruptedPacket)?;

                if self.obu_count == 0 && len == reader.remaining_bytes() {
                    is_last_obu = true;
                }

                len
            } else {
                reader.remaining_bytes()
            };

            // Safety checks
            if fragment_obu_length == 0 {
                return Err(PacketError::ErrAv1CorruptedPacket);
            }
            if reader.byte_offset() > packet.len()
                || fragment_obu_length > packet.len() - reader.byte_offset()
            {
                return Err(PacketError::ErrAv1CorruptedPacket);
            }

            if is_first_obu && self.z {
                // the previous fragment is lost, drop the buffer
                if self.obu_buffer.is_empty() {
                    reader.skip_bytes(fragment_obu_length);
                    obu_idx = 1;
                    continue;
                }
            }

            let offset = reader.byte_offset();
            self.obu_buffer
                .extend_from_slice(&packet[offset..offset + fragment_obu_length]);
            reader.skip_bytes(fragment_obu_length);

            if is_last_obu && self.y {
                self.obu_length += fragment_obu_length;
                break;
            }

            self.obu_length += fragment_obu_length;

            parse_obus(&self.obu_buffer, &mut self.parsed_obus)?;
            let Some(obu) = self.parsed_obus.first_mut() else {
                self.obu_length = 0;
                self.obu_buffer.clear();
                obu_idx += 1;
                continue;
            };

            // Write the obu payload to output
            // set size flag
            let size_flag_set = obu.header & OBU_SIZE_PRESENT_MASK != 0;
            obu.header |= OBU_SIZE_PRESENT_MASK;
            out.push(obu.header);
            self.obu_length = self.obu_length.saturating_sub(1);

            // add extension if any
            if obu.has_extension() {
                out.push(obu.ext_header);
                self.obu_length = self.obu_length.saturating_sub(1);
            }

            // add size if not present
            if !size_flag_set {
                let mut temp_space = [0u8; 9];
                let bytes_written = encode_leb_u63(self.obu_length as u64, &mut temp_space[..]);
                out.extend_from_slice(&temp_space[..bytes_written]);
            }

            // finally payload
            if self.obu_length > self.obu_buffer.len() {
                return Err(PacketError::ErrAv1CorruptedPacket);
            }
            let start_idx = self.obu_buffer.len() - self.obu_length;
            out.extend_from_slice(&self.obu_buffer[start_idx..start_idx + self.obu_length]);

            self.obu_length = 0;
            self.obu_buffer.clear();

            if is_last_obu {
                self.obu_length = 0;
                self.obu_buffer.clear();
                break;
            }

            obu_idx += 1;
        }

        Ok(())
    }

    fn is_partition_head(&self, packet: &[u8]) -> bool {
        if packet.is_empty() {
            return false;
        }

        // Z bit in the aggregation header is set to 0
        packet[0] & (1 << 7) == 0
    }

    fn is_partition_tail(&self, marker: bool, _payload: &[u8]) -> bool {
        marker
    }
}

#[repr(u8)]
enum ObuType {
    SequenceHeader = 1,
    TemporalDelimiter = 2,
    FrameHeader = 3,
    TileGroup = 4,
    MetaData = 5,
    Frame = 6,
    RedundantFrameHeader = 7,
    TileList = 8,
    Padding = 15,
}

impl ObuType {
    fn include_in_packetization(self) -> bool {
        !matches!(
            self,
            Self::TemporalDelimiter | Self::TileList | Self::Padding
        )
    }
}

/// Represents an OBU (Open Bitstream Unit) with its header, extension, payload, and size.
#[derive(Default, Debug)]
struct Obu {
    header: u8,
    ext_header: u8,
    payload: Vec<u8>,
    size: usize,
}

impl Obu {
    fn has_extension(&self) -> bool {
        (self.header & OBU_EXTENSION_PRESENT_MASK) != 0
    }

    fn has_size(&self) -> bool {
        (self.header & OBU_SIZE_PRESENT_MASK) != 0
    }

    fn obu_type(&self) -> Option<ObuType> {
        match (self.header & OBU_TYPE_MASK) >> 3 {
            1 => Some(ObuType::SequenceHeader),
            2 => Some(ObuType::TemporalDelimiter),
            3 => Some(ObuType::FrameHeader),
            4 => Some(ObuType::TileGroup),
            5 => Some(ObuType::MetaData),
            6 => Some(ObuType::Frame),
            7 => Some(ObuType::RedundantFrameHeader),
            8 => Some(ObuType::TileList),
            15 => Some(ObuType::Padding),
            _ => None,
        }
    }
}

/// The `Packet` struct holds information about the OBU (Open Bitstream Unit) and
/// related metadata necessary for constructing an RTP packet payload during packetization
#[derive(Debug)]
struct Packet {
    first_obu: usize,
    num_obu_elements: usize,
    first_obu_offset: usize,
    last_obu_size: usize,
    packet_size: usize,
}

impl Packet {
    pub fn new(first_obu: usize) -> Self {
        Packet {
            first_obu,
            num_obu_elements: 0,
            first_obu_offset: 0,
            last_obu_size: 0,
            packet_size: 0,
        }
    }
}

/// Returns the number of bytes required to encode a value using unsigned LEB128 encoding.
/// LEB128 uses variable-length encoding with 7 bits per byte, and this function computes
/// how many bytes are needed to represent the given `value`.
pub fn leb_128_size(mut value: usize) -> usize {
    let mut size = 0;

    while value >= 0x80 {
        size += 1;
        value >>= 7;
    }

    size + 1
}

/// OBU Header Structure (8 bits total):
///
///  0 1 2 3 4 5 6 7
/// +-+-+-+-+-+-+-+-+
/// |F|   T   |S|E|R|
/// +-+-+-+-+-+-+-+-+
///
/// Explanation of each bit:
/// - F (1 bit) - OBU Forbidden Bit: must be set to 0.
/// - T (4 bits) - OBU Type: This field specifies the type of data structure contained in the OBU payload.
/// - S (1 bit) - OBU Has Size Field: A flag indicating whether the obu_size syntax element will be present.
/// - E (1 bit) - OBU Extension Flag: A flag indicatin if the optional obu_extension_header is present.
/// - R (1 bit) - OBU Reserved Bits: must be set to 0. The value is ignored by a decoder.
fn parse_obus(payload: &[u8], parsed_obus: &mut Vec<Obu>) -> Result<(), PacketError> {
    let mut reader = (payload, 0);
    let mut obu_idx = 0;

    while reader.remaining_bits() > 0 {
        // Reuse existing Obu entry if available, otherwise grow the Vec
        if obu_idx >= parsed_obus.len() {
            parsed_obus.push(Obu::default());
        }
        let obu = &mut parsed_obus[obu_idx];

        obu.header = reader.get_u8().ok_or(PacketError::ErrAv1CorruptedPacket)?;
        obu.ext_header = 0;
        obu.size = 1;

        if obu.has_extension() {
            obu.ext_header = reader.get_u8().ok_or(PacketError::ErrAv1CorruptedPacket)?;
            obu.size += 1;
        }

        // Reuse the existing payload Vec — clear preserves capacity
        obu.payload.clear();

        if obu.has_size() {
            let obu_size = reader
                .get_leb128()
                .ok_or(PacketError::ErrAv1CorruptedPacket)?;
            if obu_size > reader.remaining_bytes() {
                return Err(PacketError::ErrAv1CorruptedPacket);
            }
            let bytes = reader
                .get_bytes(obu_size)
                .ok_or(PacketError::ErrAv1CorruptedPacket)?;
            obu.payload.extend_from_slice(bytes);
        } else {
            obu.payload.extend_from_slice(reader.get_remaining());
        }
        obu.size += obu.payload.len();

        if let Some(obu_type) = obu.obu_type() {
            if obu_type.include_in_packetization() {
                obu_idx += 1;
            }
        }
    }

    // Trim any leftover entries from previous calls
    parsed_obus.truncate(obu_idx);
    Ok(())
}

#[cfg(test)]
mod test {
    use super::*;

    /// AV1 Packetizer tets
    #[test]
    fn packetize_one_frame_type_obu_without_size() {
        let payload = &[0x30, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07];
        let mut packetizer = Av1Packetizer::default();

        let result = packetizer.packetize(1200, payload);

        assert!(result.is_ok());
        assert_eq!(
            result.unwrap(),
            [[0x10, 0x30, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07]]
        );
    }

    #[test]
    fn packetize_one_frame_type_obu_without_size_with_extension() {
        let payload = &[0x34, 0x28, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07];
        let mut packetizer = Av1Packetizer::default();

        let result = packetizer.packetize(1200, payload);

        assert!(result.is_ok());
        assert_eq!(
            result.unwrap(),
            [[0x10, 0x34, 0x28, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07]]
        );
    }

    #[test]
    fn packetize_one_frame_type_obu_remove_size_field_without_extension() {
        let payload = &[0x32, 0x07, 0x0b, 0x0c, 0x0d, 0x0e, 0x0f, 0x10, 0x11];
        let mut packetizer = Av1Packetizer::default();

        let result = packetizer.packetize(1200, payload);

        assert!(result.is_ok());
        assert_eq!(
            result.unwrap(),
            [[0x10, 0x30, 0x0b, 0x0c, 0x0d, 0x0e, 0x0f, 0x10, 0x11]]
        );
    }

    #[test]
    fn packetize_one_frame_type_obu_remove_size_field_with_extension() {
        let payload = &[0x36, 0x28, 0x07, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07];
        let mut packetizer = Av1Packetizer::default();

        let result = packetizer.packetize(1200, payload);

        assert!(result.is_ok());
        assert_eq!(
            result.unwrap(),
            [[0x10, 0x34, 0x28, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07]]
        );
    }

    #[test]
    fn omit_size_for_last_obu_when_three_obus_fits_into_the_packet() {
        let payload = &[
            0x0a, 0x06, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, // sequence header
            0x2a, 0x04, 0x0b, 0x0c, 0x0d, 0x0e, // metadata
            0x32, 0x06, 0x15, 0x16, 0x17, 0x18, 0x19, 0x1a, // frame
        ];
        let mut packetizer = Av1Packetizer::default();

        let result = packetizer.packetize(1200, payload);

        assert!(result.is_ok());
        assert_eq!(
            result.unwrap(),
            [[
                0x38, 0x07, 0x08, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x05, 0x28, 0x0b, 0x0c, 0x0d,
                0x0e, 0x30, 0x15, 0x16, 0x17, 0x18, 0x19, 0x1a
            ]]
        );
    }

    #[test]
    fn use_size_for_all_obus_when_four_obus_fit_into_the_packet() {
        let payload = &[
            0x0a, 0x06, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, // sequence header
            0x2a, 0x04, 0x0b, 0x0c, 0x0d, 0x0e, // metadata
            0x1a, 0x03, 0x15, 0x16, 0x17, // frame
            0x22, 0x06, 0x1f, 0x20, 0x21, 0x22, 0x23, 0x24, // tile group
        ];
        let mut packetizer = Av1Packetizer::default();

        let result = packetizer.packetize(1200, payload);

        assert!(result.is_ok());
        assert_eq!(
            result.unwrap(),
            [[
                0x08, 0x07, 0x08, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x05, 0x28, 0x0b, 0x0c, 0x0d,
                0x0e, 0x04, 0x18, 0x15, 0x16, 0x17, 0x07, 0x20, 0x1f, 0x20, 0x21, 0x22, 0x23, 0x24
            ]]
        );
    }

    #[test]
    fn discards_temporal_delimiter_and_tile_list_obu() {
        let payload = &[
            0x12, 0x00, // temporal delimeter
            0x2a, 0x00, // metadata
            0x42, 0x06, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, // tile list
            0x1a, 0x03, 0x15, 0x16, 0x17, // frame
            0x22, 0x06, 0x1f, 0x20, 0x21, 0x22, 0x23, 0x24, // tile group
        ];
        let mut packetizer = Av1Packetizer::default();

        let result = packetizer.packetize(1200, payload);

        assert!(result.is_ok());
        assert_eq!(
            result.unwrap(),
            [[
                0x30, 0x01, 0x28, 0x04, 0x18, 0x15, 0x16, 0x17, 0x20, 0x1f, 0x20, 0x21, 0x22, 0x23,
                0x24
            ]]
        );
    }

    #[test]
    fn split_two_obus_into_two_packets_force_split_obu_header() {
        let payload = &[
            0x1e, 0x28, 0x01, 0x15, // frame
            0x26, 0x28, 0x04, 0x0b, 0x0c, 0x0d, 0x0e, // tile group
        ];
        let mut packetizer = Av1Packetizer::default();

        let result = packetizer.packetize(6, payload);

        assert!(result.is_ok());
        assert_eq!(
            result.unwrap(),
            vec![
                vec![0x60, 0x03, 0x1c, 0x28, 0x15, 0x24],
                vec![0x90, 0x28, 0x0b, 0x0c, 0x0d, 0x0e]
            ]
        );
    }

    #[test]
    fn set_nbit_at_the_first_packet_of_coded_video_sequence() {}

    #[test]
    fn split_single_obu_into_two_packets() {
        let payload = &[
            0x32, 0x09, 0x0b, 0x0c, 0x0d, 0x0e, 0x0f, 0x10, 0x11, 0x12, 0x13,
        ];
        let mut packetizer = Av1Packetizer::default();

        let result = packetizer.packetize(8, payload);

        assert!(result.is_ok());
        assert_eq!(
            result.unwrap(),
            vec![
                vec![0x50, 0x30, 0x0b, 0x0c, 0x0d, 0x0e, 0x0f, 0x10],
                vec![0x90, 0x11, 0x12, 0x13]
            ]
        );
    }

    #[test]
    fn split_single_obu_into_many_packets() {
        let mut payload: Vec<u8> = vec![0x32, 0xB0, 0x09]; // obu header and leb128 encoded size
        payload.extend(vec![27u8; 1200]); // obu payload

        let mut packetizer = Av1Packetizer::default();

        let result = packetizer.packetize(100, &payload);

        assert!(result.is_ok());
        assert_eq!(result.unwrap().len(), 13);
    }

    #[test]
    fn split_two_obus_into_two_packets() {
        let payload = &[
            0x0a, 0x02, 0x0b, 0x0c, // sequence header
            0x32, 0x09, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, // frame
        ];
        let mut packetizer = Av1Packetizer::default();

        let result = packetizer.packetize(8, payload);

        assert!(result.is_ok());
        assert_eq!(
            result.unwrap(),
            vec![
                vec![0x68, 0x03, 0x08, 0x0b, 0x0c, 0x30, 0x01, 0x02],
                vec![0x90, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09]
            ]
        );
    }

    #[test]
    fn split_single_obu_into_two_packets_because_of_mtu_limit() {
        let payload = &[
            0x32, 0x09, 0x0b, 0x0c, 0x0d, 0x0e, 0x0f, 0x10, 0x11, 0x12, 0x13,
        ];
        let mut packetizer = Av1Packetizer::default();

        let result = packetizer.packetize(10, payload);

        assert!(result.is_ok());
        assert_eq!(
            result.unwrap(),
            vec![
                vec![0x50, 0x30, 0x0b, 0x0c, 0x0d, 0x0e, 0x0f, 0x10, 0x11, 0x12],
                vec![0x90, 0x13]
            ]
        );
    }

    /// AV1 Depacketizer tests
    #[test]
    fn obu_payload_size_set_when_absent() {
        let paylod = &[0x10, 0x30, 0x14, 0x1e, 0x28];
        let mut out = Vec::new();
        let mut codec_extra = CodecExtra::None;
        let mut depacketizer = Av1Depacketizer::default();

        let result = depacketizer.depacketize(paylod, &mut out, &mut codec_extra);

        assert!(result.is_ok());
        assert_eq!(out, vec![0x32, 0x03, 0x14, 0x1e, 0x28]);
    }

    #[test]
    fn obu_payload_size_set_when_present() {
        let paylod = &[0x10, 0x32, 0x03, 0x14, 0x1e, 0x28];
        let mut out = Vec::new();
        let mut codec_extra = CodecExtra::None;
        let mut depacketizer = Av1Depacketizer::default();

        let result = depacketizer.depacketize(paylod, &mut out, &mut codec_extra);

        assert!(result.is_ok());
        assert_eq!(out, vec![0x32, 0x03, 0x14, 0x1e, 0x28]);
    }

    #[test]
    fn obu_payload_size_set_after_extension_when_absent() {
        let paylod = &[0x10, 0x34, 0x48, 0x14, 0x1e, 0x28];
        let mut out = Vec::new();
        let mut codec_extra = CodecExtra::None;
        let mut depacketizer = Av1Depacketizer::default();

        let result = depacketizer.depacketize(paylod, &mut out, &mut codec_extra);

        assert!(result.is_ok());
        assert_eq!(out, vec![0x36, 0x48, 0x03, 0x14, 0x1e, 0x28]);
    }

    #[test]
    fn obu_payload_size_set_after_extension_when_present() {
        let paylod = &[0x10, 0x36, 0x48, 0x03, 0x14, 0x1e, 0x28];
        let mut out = Vec::new();
        let mut codec_extra = CodecExtra::None;
        let mut depacketizer = Av1Depacketizer::default();

        let result = depacketizer.depacketize(paylod, &mut out, &mut codec_extra);

        assert!(result.is_ok());
        assert_eq!(out, vec![0x36, 0x48, 0x03, 0x14, 0x1e, 0x28]);
    }

    #[test]
    fn one_packet_with_two_obus() {
        let paylod = &[0x20, 0x02, 0x08, 0x0a, 0x30, 0x14];
        let mut out = Vec::new();
        let mut codec_extra = CodecExtra::None;
        let mut depacketizer = Av1Depacketizer::default();

        let result = depacketizer.depacketize(paylod, &mut out, &mut codec_extra);

        assert!(result.is_ok());
        assert_eq!(out, vec![0x0a, 0x01, 0x0a, 0x32, 0x01, 0x14]);
    }

    #[test]
    fn one_obu_from_two_packets() {
        let payload1 = &[0x50, 0x30, 0x14, 0x1e];
        let payload2 = &[0x90, 0x28];
        let mut out = Vec::new();
        let mut codec_extra = CodecExtra::None;
        let mut depacketizer = Av1Depacketizer::default();

        let result = depacketizer.depacketize(payload1, &mut out, &mut codec_extra);
        assert!(result.is_ok());

        let result = depacketizer.depacketize(payload2, &mut out, &mut codec_extra);
        assert!(result.is_ok());
        assert_eq!(out, vec![0x32, 0x03, 0x14, 0x1e, 0x28]);
    }

    #[test]
    fn two_packets_with_three_obus() {
        let payload1 = &[0x60, 0x02, 0x08, 0x0a, 0x30, 0x14, 0x1e];
        let payload2 = &[0x90, 0x28];
        let mut out = Vec::new();
        let mut codec_extra = CodecExtra::None;
        let mut depacketizer = Av1Depacketizer::default();

        let result = depacketizer.depacketize(payload1, &mut out, &mut codec_extra);
        assert!(result.is_ok());

        let result = depacketizer.depacketize(payload2, &mut out, &mut codec_extra);
        assert!(result.is_ok());
        assert_eq!(out, vec![0x0a, 0x01, 0x0a, 0x32, 0x03, 0x14, 0x1e, 0x28]);
    }

    #[test]
    fn two_packets_with_many_obus_some_with_extensions() {
        let payload1 = &[
            0x40, 0x02, 0x08, 0x0a, 0x02, 0x28, 0x14, 0x04, 0x2c, 0x30, 0x14, 0x1e, 0x05, 0x34,
            0x30, 0x28, 0x32, 0x3c,
        ];
        let payload2 = &[0x90, 0x46, 0x50, 0x5a];
        let mut out = Vec::new();
        let mut codec_extra = CodecExtra::None;
        let mut depacketizer = Av1Depacketizer::default();

        let result = depacketizer.depacketize(payload1, &mut out, &mut codec_extra);
        assert!(result.is_ok());

        let result = depacketizer.depacketize(payload2, &mut out, &mut codec_extra);
        assert!(result.is_ok());
        assert_eq!(
            out,
            vec![
                0x0a, 0x01, 0x0a, 0x2a, 0x01, 0x14, 0x2e, 0x30, 0x02, 0x14, 0x1e, 0x36, 0x30, 0x06,
                0x28, 0x32, 0x3c, 0x46, 0x50, 0x5a
            ]
        );
    }

    #[test]
    fn one_obu_from_many_packets() {
        let payload1 = &[0x50, 0x30, 0x0b, 0x0c];
        let payload2 = &[0xd0, 0x0d, 0x0e];
        let payload3 = &[0xd0, 0x0f, 0x10, 0x11];
        let payload4 = &[0x90, 0x12];
        let mut out = Vec::new();
        let mut codec_extra = CodecExtra::None;
        let mut depacketizer = Av1Depacketizer::default();

        let result = depacketizer.depacketize(payload1, &mut out, &mut codec_extra);
        assert!(result.is_ok());
        let result = depacketizer.depacketize(payload2, &mut out, &mut codec_extra);
        assert!(result.is_ok());
        let result = depacketizer.depacketize(payload3, &mut out, &mut codec_extra);
        assert!(result.is_ok());
        let result = depacketizer.depacketize(payload4, &mut out, &mut codec_extra);
        assert!(result.is_ok());
        assert_eq!(
            out,
            vec![0x32, 0x08, 0x0b, 0x0c, 0x0d, 0x0e, 0x0f, 0x10, 0x11, 0x12]
        );
    }

    #[test]
    fn many_packets_with_border_aligned_obus() {
        let payload1 = &[0x60, 0x03, 0x18, 0x0b, 0x0c, 0x20, 0x15, 0x16, 0x17];
        let payload2 = &[0x90, 0x18, 0x19, 0x1a, 0x1b];
        let payload3 = &[0x60, 0x03, 0x38, 0x0b, 0x0c, 0x20, 0x1f, 0x20];
        let payload4 = &[0x90, 0x21, 0x22, 0x23, 0x24];
        let mut out = Vec::new();
        let mut codec_extra = CodecExtra::None;
        let mut depacketizer = Av1Depacketizer::default();

        let result = depacketizer.depacketize(payload1, &mut out, &mut codec_extra);
        assert!(result.is_ok());
        let result = depacketizer.depacketize(payload2, &mut out, &mut codec_extra);
        assert!(result.is_ok());
        let result = depacketizer.depacketize(payload3, &mut out, &mut codec_extra);
        assert!(result.is_ok());
        let result = depacketizer.depacketize(payload4, &mut out, &mut codec_extra);
        assert!(result.is_ok());
        assert_eq!(
            out,
            vec![
                0x1a, 0x02, 0x0b, 0x0c, 0x22, 0x07, 0x15, 0x16, 0x17, 0x18, 0x19, 0x1a, 0x1b, 0x3a,
                0x02, 0x0b, 0x0c, 0x22, 0x06, 0x1f, 0x20, 0x21, 0x22, 0x23, 0x24
            ]
        );
    }

    #[test]
    fn one_packet_one_obu_with_payload_size_127_bytes() {
        let mut payload = [0; 131];
        let mut out = Vec::new();
        let mut codec_extra = CodecExtra::None;
        let mut depacketizer = Av1Depacketizer::default();

        payload[0] = 0b0000_0000; // aggregation header
        payload[1] = 0x80; // leb128 encoded size of 128 bytes
        payload[2] = 0x01; // in two bytes
        payload[3] = 0b0011_0000; // obu_header with size and extension bits unset.
        payload[4 + 42] = 0x42; // random

        let result = depacketizer.depacketize(&payload, &mut out, &mut codec_extra);
        assert!(result.is_ok());
        assert_eq!(out[0], 0b0011_0010); // size bit set
        assert_eq!(out[1], 127); // obu payloa size, 1 byte is enough
        assert_eq!(out[44], 0x42); // check random byte
    }

    #[test]
    fn two_packets_one_obu_with_payload_size_128_bytes() {
        let mut payload1 = [0; 35];
        let mut payload2 = [0; 98];
        let mut out = Vec::new();
        let mut codec_extra = CodecExtra::None;
        let mut depacketizer = Av1Depacketizer::default();

        payload1[0] = 0b0100_0000; // aggregation header
        payload1[1] = 33; // payload size
        payload1[2] = 0b0011_0000; // obu_header with size and extension bits unset.
        payload1[3 + 10] = 0x10; // random

        payload2[0] = 0b1000_0000;
        payload2[1] = 96;
        payload2[2 + 20] = 0x20;

        let result = depacketizer.depacketize(&payload1, &mut out, &mut codec_extra);
        assert!(result.is_ok());
        let result = depacketizer.depacketize(&payload2, &mut out, &mut codec_extra);
        assert!(result.is_ok());
        assert_eq!(out[0], 0b0011_0010);
        assert_eq!(out[1], 0x80);
        assert_eq!(out[2], 0x01);
        assert_eq!(out[3 + 10], 0x10);
        assert_eq!(out[3 + 32 + 20], 0x20);
    }

    // Regression tests for leb_128_size boundary values.
    // The bug was `value > 0x80` instead of `value >= 0x80`.
    // LEB128 encodes 7 bits per byte, so values >= 128 need 2 bytes,
    // values >= 16384 need 3 bytes, etc.

    #[test]
    fn leb128_size_boundary_at_128() {
        // 127 fits in 1 byte (7 bits), 128 needs 2 bytes
        assert_eq!(leb_128_size(0), 1);
        assert_eq!(leb_128_size(1), 1);
        assert_eq!(leb_128_size(127), 1);
        assert_eq!(leb_128_size(128), 2); // was incorrectly 1 before fix
        assert_eq!(leb_128_size(129), 2);
        assert_eq!(leb_128_size(255), 2);
    }

    #[test]
    fn leb128_size_boundary_at_16384() {
        // 16383 fits in 2 bytes (14 bits), 16384 needs 3 bytes
        assert_eq!(leb_128_size(16383), 2);
        assert_eq!(leb_128_size(16384), 3); // was incorrectly 2 before fix
        assert_eq!(leb_128_size(16385), 3);
    }

    #[test]
    fn leb128_size_boundary_at_2097152() {
        // 2097151 fits in 3 bytes (21 bits), 2097152 needs 4 bytes
        assert_eq!(leb_128_size(2097151), 3);
        assert_eq!(leb_128_size(2097152), 4); // was incorrectly 3 before fix
    }

    // Regression test: packetize an OBU with exactly 128 bytes of payload.
    // This triggers the leb_128_size boundary (128 needs 2-byte LEB128).
    // Before the fix, leb_128_size(128) returned 1 instead of 2, causing
    // the output buffer to be 1 byte too small → panic on slice indexing.
    #[test]
    fn packetize_obu_with_128_byte_payload_no_panic() {
        // Build a raw OBU: header (0x32 = frame, size present)
        // + LEB128 size (0x80, 0x01 = 128) + 128 bytes payload
        let mut payload = Vec::with_capacity(3 + 128);
        payload.push(0x32); // OBU header: frame type, size present
        payload.push(0x80); // LEB128 size byte 1: 128
        payload.push(0x01); // LEB128 size byte 2
        payload.extend(vec![0xAB; 128]); // 128 bytes of payload

        let mut packetizer = Av1Packetizer::default();
        let result = packetizer.packetize(1200, &payload);
        assert!(result.is_ok());
        let packets = result.unwrap();
        assert!(!packets.is_empty());
    }

    // Regression test: packetize an OBU that requires fragmentation across
    // multiple RTP packets, with exactly 128 bytes of payload.
    // This exercises both the leb_128_size fix (buffer allocation) and
    // the payload_offset fix (correct data in continuation fragments).
    #[test]
    fn packetize_128_byte_obu_fragmented_correctly() {
        // OBU: header + LEB128(128) + 128 bytes payload
        let mut payload = Vec::with_capacity(3 + 128);
        payload.push(0x32); // frame type, size present
        payload.push(0x80); // LEB128: 128
        payload.push(0x01);
        for i in 0u8..128 {
            payload.push(i); // distinguishable payload bytes
        }

        let mut packetizer = Av1Packetizer::default();
        // MTU of 50 forces fragmentation into multiple packets
        let result = packetizer.packetize(50, &payload);
        assert!(result.is_ok());
        let packets = result.unwrap();
        assert!(
            packets.len() >= 3,
            "128-byte OBU with MTU 50 should produce at least 3 packets"
        );

        // Verify round-trip: depacketize all packets and check payload integrity
        let mut out = Vec::new();
        let mut codec_extra = CodecExtra::None;
        let mut depacketizer = Av1Depacketizer::default();
        for pkt in &packets {
            let r = depacketizer.depacketize(pkt, &mut out, &mut codec_extra);
            assert!(r.is_ok(), "depacketize failed: {:?}", r);
        }
        // Output should be: OBU header (with size bit) + LEB128 size + payload
        // Find the payload portion and verify it matches
        assert!(
            out.len() >= 128,
            "round-tripped output too short: {} bytes",
            out.len()
        );
        // The payload bytes should appear in order at the end
        let payload_start = out.len() - 128;
        for i in 0u8..128 {
            assert_eq!(
                out[payload_start + i as usize],
                i,
                "payload byte {} mismatch after round-trip",
                i
            );
        }
    }

    // Regression test for the payload_offset bug in continuation fragments.
    // When an OBU with extension header spans multiple packets, the second
    // fragment used `obu_offset` instead of `payload_offset` to compute the
    // remaining payload size, causing incorrect data or panics.
    #[test]
    fn packetize_obu_with_extension_fragmented_round_trip() {
        // OBU with extension: header=0x36 (frame type + extension + size present),
        // ext_header=0x10, LEB128 size, then payload
        let mut raw = Vec::with_capacity(4 + 200);
        raw.push(0x36); // OBU header: frame, extension present, size present
        raw.push(0x10); // extension header
        raw.push(0xC8); // LEB128 size: 200
        raw.push(0x01);
        for i in 0u8..200 {
            raw.push(i);
        }

        let mut packetizer = Av1Packetizer::default();
        let result = packetizer.packetize(40, &raw);
        assert!(result.is_ok());
        let packets = result.unwrap();
        assert!(
            packets.len() >= 5,
            "200-byte OBU with MTU 40 should produce multiple packets"
        );

        // Round-trip through depacketizer
        let mut out = Vec::new();
        let mut codec_extra = CodecExtra::None;
        let mut depacketizer = Av1Depacketizer::default();
        for pkt in &packets {
            depacketizer
                .depacketize(pkt, &mut out, &mut codec_extra)
                .unwrap();
        }

        // Verify the 200-byte payload is intact
        let payload_start = out.len() - 200;
        for i in 0u8..200 {
            assert_eq!(
                out[payload_start + i as usize],
                i,
                "extension OBU payload byte {} corrupted after round-trip",
                i
            );
        }
    }

    #[test]
    fn test_detect_av1_keyframe() {
        // Empty
        assert!(!detect_av1_keyframe(&[]));

        // AV1 aggregation header: Z|Y|W W|N|reserved
        // N bit is bit 3 (0x08)

        // N=1 → keyframe
        assert!(detect_av1_keyframe(&[0x08]));
        assert!(detect_av1_keyframe(&[0x18])); // Z=0,Y=0,W=01,N=1
        assert!(detect_av1_keyframe(&[0x78])); // Z=0,Y=1,W=11,N=1
        assert!(detect_av1_keyframe(&[0x88])); // Z=1,Y=0,W=00,N=1
        assert!(detect_av1_keyframe(&[0x0F])); // N=1, reserved bits set

        // N=0 → not a keyframe
        assert!(!detect_av1_keyframe(&[0x00]));
        assert!(!detect_av1_keyframe(&[0x10])); // W=01, N=0
        assert!(!detect_av1_keyframe(&[0x70])); // Y=1, W=11, N=0
        assert!(!detect_av1_keyframe(&[0xF0])); // Z=1, Y=1, W=11, N=0
    }

    #[test]
    fn packetize_respects_mtu() {
        // OBU header (frame-type, no size field) + payload bytes.
        let mut payload = vec![0x30u8];
        payload.extend(std::iter::repeat_n(0xABu8, 2000));
        for &mtu in &[100usize, 300, 600, 1200] {
            let mut packetizer = Av1Packetizer::default();
            let pkts = packetizer
                .packetize(mtu, &payload)
                .expect("AV1 packetize ok");
            assert!(!pkts.is_empty(), "AV1 produced no packets at mtu {mtu}");
            for (i, pkt) in pkts.iter().enumerate() {
                assert!(
                    pkt.len() <= mtu,
                    "AV1 packet {i} size {} > mtu {mtu}",
                    pkt.len()
                );
            }
        }
    }
}
