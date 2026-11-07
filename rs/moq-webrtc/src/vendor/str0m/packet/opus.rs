use super::{CodecExtra, Depacketizer, PacketError, Packetizer};

/// Packetizes Opus RTP packets.
///
/// ## Unversioned API surface
///
/// This struct is not currently versioned according to semver rules.
/// Breaking changes may be made in minor or patch releases.
#[derive(Default, Debug, Copy, Clone)]
pub struct OpusPacketizer;

impl Packetizer for OpusPacketizer {
    fn packetize(&mut self, mtu: usize, payload: &[u8]) -> Result<Vec<Vec<u8>>, PacketError> {
        if payload.is_empty() || mtu == 0 {
            return Ok(vec![]);
        }

        let mut out = vec![];

        let mut cur = 0;
        while cur < payload.len() {
            let min = mtu.min(payload.len() - cur);
            out.push(payload[cur..(cur + min)].to_vec());
            cur += mtu;
        }

        Ok(out)
    }

    fn is_marker(&mut self, _data: &[u8], _previous: Option<&[u8]>, _last: bool) -> bool {
        // TODO: dtx
        false
    }

    fn marks_talkspurt(&self) -> bool {
        true
    }

    fn nackable(&self) -> bool {
        false
    }
}

/// Depacketizes Opus RTP packets.
///
/// ## Unversioned API surface
///
/// This struct is not currently versioned according to semver rules.
/// Breaking changes may be made in minor or patch releases.
#[derive(PartialEq, Eq, Debug, Default, Clone)]
pub struct OpusDepacketizer;

impl Depacketizer for OpusDepacketizer {
    fn out_size_hint(&self, packets_size: usize) -> Option<usize> {
        Some(packets_size)
    }

    fn depacketize(
        &mut self,
        packet: &[u8],
        out: &mut Vec<u8>,
        _: &mut CodecExtra,
    ) -> Result<(), PacketError> {
        if !packet.is_empty() {
            out.extend_from_slice(packet);
        }

        Ok(())
    }

    fn is_partition_head(&self, _payload: &[u8]) -> bool {
        true
    }

    fn is_partition_tail(&self, _marker: bool, _payload: &[u8]) -> bool {
        true
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn test_opus_unmarshal() -> Result<(), PacketError> {
        let mut pck = OpusDepacketizer;
        let mut extra = CodecExtra::None;

        // Empty packet
        let empty_bytes = &[];
        let mut out = Vec::new();
        let result = pck.depacketize(empty_bytes, &mut out, &mut extra);
        assert!(
            result.is_ok(),
            "Result should not be err in case of empty packet"
        );

        // Normal packet
        let raw_bytes: &[u8] = &[0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x90];
        let mut out = Vec::new();
        pck.depacketize(raw_bytes, &mut out, &mut extra)?;
        assert_eq!(raw_bytes, &out, "Payload must be same");

        Ok(())
    }

    #[test]
    fn test_opus_payload() -> Result<(), PacketError> {
        let mut pck = OpusPacketizer;
        let empty = &[];
        let payload = &[0x90, 0x90, 0x90];

        // Positive MTU, empty payload
        let result = pck.packetize(1, empty)?;
        assert!(result.is_empty(), "Generated payload should be empty");

        // Positive MTU, small payload
        let result = pck.packetize(1, payload)?;
        assert_eq!(result.len(), 3, "Generated payload should be 3");

        // Positive MTU, small payload
        let result = pck.packetize(2, payload)?;
        assert_eq!(result.len(), 2, "Generated payload should be the 2");

        Ok(())
    }

    #[test]
    fn test_opus_is_partition_head() -> Result<(), PacketError> {
        let opus = OpusDepacketizer;
        //"NormalPacket"
        assert!(
            opus.is_partition_head(&[0x00, 0x00]),
            "All OPUS RTP packet should be the head of a new partition"
        );

        Ok(())
    }

    #[test]
    fn packetize_respects_mtu() -> Result<(), PacketError> {
        let payload = vec![0xABu8; 2000];
        for &mtu in &[100usize, 300, 600, 1200] {
            let mut pck = OpusPacketizer;
            let pkts = pck.packetize(mtu, &payload)?;
            assert!(!pkts.is_empty(), "Opus produced no packets at mtu {mtu}");
            for (i, pkt) in pkts.iter().enumerate() {
                assert!(
                    pkt.len() <= mtu,
                    "Opus packet {i} size {} > mtu {mtu}",
                    pkt.len()
                );
            }
        }
        Ok(())
    }
}
