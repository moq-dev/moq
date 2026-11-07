use super::{CodecExtra, Depacketizer, PacketError, Packetizer};

#[derive(Debug)]
pub struct ComfortNoisePacketizer;

#[derive(Debug)]
pub struct ComfortNoiseDepacketizer;

impl Packetizer for ComfortNoisePacketizer {
    fn packetize(&mut self, mtu: usize, payload: &[u8]) -> Result<Vec<Vec<u8>>, PacketError> {
        if payload.is_empty() {
            return Ok(vec![]);
        }

        if payload.len() > mtu {
            return Err(PacketError::ErrPayloadTooLarge);
        }

        Ok(vec![payload.to_vec()])
    }

    fn is_marker(&mut self, _data: &[u8], _previous: Option<&[u8]>, _last: bool) -> bool {
        false
    }

    // Comfort noise deliberately does not set the talkspurt marker (unlike other audio), so it
    // keeps the default `marks_talkspurt() == false`.

    fn nackable(&self) -> bool {
        false
    }
}

impl Depacketizer for ComfortNoiseDepacketizer {
    fn out_size_hint(&self, packets_size: usize) -> Option<usize> {
        Some(packets_size)
    }

    fn depacketize(
        &mut self,
        packet: &[u8],
        out: &mut Vec<u8>,
        _codec_extra: &mut CodecExtra,
    ) -> Result<(), PacketError> {
        if packet.is_empty() {
            return Err(PacketError::ErrShortPacket);
        }

        out.extend_from_slice(packet);
        Ok(())
    }

    fn is_partition_head(&self, _packet: &[u8]) -> bool {
        true
    }

    fn is_partition_tail(&self, _marker: bool, _packet: &[u8]) -> bool {
        true
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn packetizer_rejects_payload_larger_than_mtu() {
        let mut packetizer = ComfortNoisePacketizer;

        let result = packetizer.packetize(8, &[0; 9]);

        assert!(
            result.is_err(),
            "a CN payload cannot be fragmented and must not exceed the MTU"
        );
    }

    #[test]
    fn packetizer_does_not_emit_empty_cn_payload() {
        let mut packetizer = ComfortNoisePacketizer;

        let packets = packetizer.packetize(1200, &[]).unwrap();

        assert!(packets.is_empty(), "an empty input is not a CN payload");
    }

    #[test]
    fn depacketizer_rejects_empty_cn_payload() {
        let mut depacketizer = ComfortNoiseDepacketizer;
        let mut output = Vec::new();

        let result = depacketizer.depacketize(&[], &mut output, &mut CodecExtra::None);

        assert_eq!(result, Err(PacketError::ErrShortPacket));
    }
}
