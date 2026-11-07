use std::time::Duration;
use std::time::Instant;

use crate::io::DATAGRAM_MAX_PACKET_SIZE;
use crate::rtp_::SeqNo;

use super::RtpPacket;
use super::rtx_cache_buf::EvictingBuffer;

const RTX_CACHE_SIZE_QUANTIZER: usize = 25;
const RTX_CACHE_QUANTIZE_SLOTS: usize = DATAGRAM_MAX_PACKET_SIZE / RTX_CACHE_SIZE_QUANTIZER;

#[derive(Debug)]
pub(crate) struct RtxCache {
    // Data, new additions here probably need to be cleared in [`clear`].
    packet_by_seq_no: EvictingBuffer<RtpPacket>,

    // Technically we want [Option<SeqNo>; X] to indicate the absence of
    // a SeqNo. However We can half the storage space by using the sentinel
    // values SeqNo::MAX to indicate None
    seq_no_by_quantized_size: [SeqNo; RTX_CACHE_QUANTIZE_SLOTS],
}

impl RtxCache {
    pub fn new(max_packet_count: usize, max_packet_age: Duration) -> Self {
        Self {
            packet_by_seq_no: EvictingBuffer::new(10, max_packet_age, max_packet_count),
            seq_no_by_quantized_size: [SeqNo::MAX; RTX_CACHE_QUANTIZE_SLOTS],
        }
    }

    pub fn cache_sent_packet(&mut self, packet: RtpPacket, now: Instant) {
        assert!(packet.nackable);
        let seq_no = packet.seq_no;
        let quantized_size = packet.payload.len() / RTX_CACHE_SIZE_QUANTIZER;
        self.packet_by_seq_no.push(*seq_no, now, packet);
        self.seq_no_by_quantized_size[quantized_size] = seq_no;
        self.remove_old_packets(now);
    }

    pub fn last_cached_seq_no(&self) -> Option<SeqNo> {
        Some(self.packet_by_seq_no.last_position()?.into())
    }

    pub fn get_cached_packet_by_seq_no(&mut self, seq_no: SeqNo) -> Option<&mut RtpPacket> {
        self.packet_by_seq_no.get_mut(*seq_no)
    }

    pub fn get_cached_packet_smaller_than(&mut self, max_size: usize) -> Option<&mut RtpPacket> {
        let quantized_size = max_size / RTX_CACHE_SIZE_QUANTIZER;

        let seq_no = self.seq_no_by_quantized_size[..quantized_size]
            .iter()
            .rev()
            .filter(|seq_no| !seq_no.is_max())
            .find(|seq_no| self.packet_by_seq_no.contains(***seq_no))?;

        self.get_cached_packet_by_seq_no(*seq_no)
    }

    fn remove_old_packets(&mut self, now: Instant) {
        self.packet_by_seq_no.maybe_evict(now);
    }

    pub(crate) fn last_packet(&self) -> Option<&[u8]> {
        let packet = self.packet_by_seq_no.last()?;
        Some(packet.payload.as_ref())
    }

    pub(crate) fn clear(&mut self) {
        self.packet_by_seq_no.clear();
        self.seq_no_by_quantized_size = [SeqNo::MAX; RTX_CACHE_QUANTIZE_SLOTS];
    }
}

#[cfg(test)]
mod test {
    use crate::rtp_::MediaTime;
    use crate::rtp_::RtpHeader;

    use super::*;

    fn after(now: Instant, millis: u64) -> Instant {
        now + Duration::from_millis(millis)
    }

    fn packet(now: Instant, seq_no: u64, millis: u64) -> RtpPacket {
        RtpPacket {
            header: RtpHeader::default(),
            seq_no: seq_no.into(),
            time: MediaTime::from_90khz(0),
            payload: millis.to_be_bytes().into(),
            vp8_patch: None,
            timestamp: after(now, millis),
            last_sender_info: None,
            nackable: true,
        }
    }

    #[test]
    fn rtx_cache_0_sized() {
        let now = Instant::now();
        let mut rtx_cache = RtxCache::new(0, Duration::from_secs(3));
        rtx_cache.cache_sent_packet(packet(now, 1, 10), after(now, 10));
        assert_eq!(
            Some(&mut packet(now, 1, 10)),
            rtx_cache.get_cached_packet_by_seq_no(1.into())
        );
        assert_eq!(
            Some(&mut packet(now, 1, 10)),
            rtx_cache.get_cached_packet_smaller_than(1000)
        );
    }

    #[test]
    fn rtx_cache_0_duration() {
        let now = Instant::now();
        let mut rtx_cache = RtxCache::new(10, Duration::from_secs(0));
        rtx_cache.cache_sent_packet(packet(now, 1, 10), after(now, 10));
        assert_eq!(None, rtx_cache.get_cached_packet_by_seq_no(1.into()));
        assert_eq!(None, rtx_cache.get_cached_packet_smaller_than(1000));
    }

    #[test]
    fn rtx_cache_1_sized() {
        let now = Instant::now();
        let mut rtx_cache = RtxCache::new(1, Duration::from_secs(3));
        rtx_cache.cache_sent_packet(packet(now, 1, 10), after(now, 10));
        assert_eq!(
            Some(&mut packet(now, 1, 10)),
            rtx_cache.get_cached_packet_by_seq_no(1.into())
        );
        assert_eq!(
            Some(&mut packet(now, 1, 10)),
            rtx_cache.get_cached_packet_smaller_than(1000)
        );
        assert_eq!(
            Some(&mut packet(now, 1, 10)),
            rtx_cache.get_cached_packet_smaller_than(25)
        );
        assert_eq!(None, rtx_cache.get_cached_packet_smaller_than(24));
        rtx_cache.cache_sent_packet(packet(now, 2, 20), after(now, 20));
        assert_eq!(
            Some(&mut packet(now, 2, 20)),
            rtx_cache.get_cached_packet_by_seq_no(2.into())
        );
    }

    #[test]
    fn rtx_cache_100_sized_backwards() {
        let now = Instant::now();
        let mut rtx_cache = RtxCache::new(100, Duration::from_secs(3));

        for i in 1..=200 {
            let seq_no = 201 - i;
            let pkt = packet(now, seq_no, (201 - i) * 10);
            let now = after(now, i * 10);
            rtx_cache.cache_sent_packet(pkt, now);
        }

        assert_eq!(
            Some(&mut packet(now, 200, 2000)),
            rtx_cache.get_cached_packet_by_seq_no(200.into())
        );
        assert_eq!(None, rtx_cache.get_cached_packet_by_seq_no(100.into()));
        assert_eq!(None, rtx_cache.get_cached_packet_by_seq_no(101.into()));
    }

    #[test]
    fn rtx_cache_eviction() {
        let now = Instant::now();
        // Cache can take all entries
        let mut rtx_cache = RtxCache::new(400, Duration::from_secs(1));
        for i in 1..=200 {
            let seq_no = i;
            let pkt = packet(now, seq_no, i * 10);
            let now = after(now, i * 10);
            rtx_cache.cache_sent_packet(pkt, now);
        }

        assert_eq!(None, rtx_cache.get_cached_packet_by_seq_no(99.into()));

        assert_eq!(
            Some(&mut packet(now, 100, 1000)),
            rtx_cache.get_cached_packet_by_seq_no(100.into())
        );
        assert_eq!(
            Some(&mut packet(now, 200, 2000)),
            rtx_cache.get_cached_packet_by_seq_no(200.into())
        );
    }
}
