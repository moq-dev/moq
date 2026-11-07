//! Builds outgoing RFC 4733 telephone packets for a complete event.

use std::time::{Duration, Instant};

use crate::packet::duration_from_units;

use super::{ExtensionValues, Frequency, MediaTime, Pt, Rid, TelephoneEvent, ToPayload};

const UPDATE_INTERVAL: Duration = Duration::from_millis(20);
pub(crate) const MIN_PAUSE: Duration = Duration::from_millis(50);
pub(crate) const MIN_DURATION: Duration = Duration::from_millis(40);
pub(crate) const MAX_DURATION: Duration = Duration::from_millis(6000);

/// The largest duration in one telephone payload. Longer events use contiguous segments.
const MAX_SEGMENT: u64 = u16::MAX as u64;

/// Generates the entire packet series without holding a temporary collection.
pub(crate) struct TelephonePackets {
    pt: Pt,
    rid: Option<Rid>,
    tele: TelephoneEvent,
    wallclock: Instant,
    rtp_time: MediaTime,
    ext_vals: ExtensionValues,
    next_update: Duration,
    segment: u64,
    first: bool,
    done: bool,
    final_repeats: u32,
}

impl TelephonePackets {
    pub(crate) fn new(
        pt: Pt,
        rid: Option<Rid>,
        tele: TelephoneEvent,
        wallclock: Instant,
        rtp_time: MediaTime,
        ext_vals: ExtensionValues,
    ) -> Self {
        Self {
            pt,
            rid,
            tele,
            wallclock,
            rtp_time,
            ext_vals,
            next_update: UPDATE_INTERVAL,
            segment: 0,
            first: true,
            done: false,
            final_repeats: 0,
        }
    }

    fn packet(&mut self, duration: u64, end: bool, not_before: Instant) -> ToPayload {
        let clock_rate = self.rtp_time.frequency();
        let payload = TelephoneEvent {
            event: self.tele.event,
            end,
            volume: self.tele.volume,
            duration: duration_from_units(duration as u16, clock_rate),
        };
        let marker = self.first;
        self.first = false;
        let wallclock = self.wallclock + Duration::from(MediaTime::new(self.segment, clock_rate));
        let rtp_time = MediaTime::new(self.rtp_time.numer() + self.segment, clock_rate);
        let data = payload
            .to_bytes(clock_rate)
            .expect("segment duration fits in 16 bits")
            .into();

        ToPayload {
            pt: self.pt,
            rid: self.rid,
            // The RTP timestamp and wallclock identify the segment start. `not_before` is the
            // independent send deadline for this packet.
            wallclock,
            rtp_time,
            not_before: Some(not_before),
            start_of_talk_spurt: marker,
            data,
            ext_vals: self.ext_vals.clone(),
        }
    }
}

impl Iterator for TelephonePackets {
    type Item = ToPayload;

    fn next(&mut self) -> Option<Self::Item> {
        if self.final_repeats > 0 {
            let repeat = 3 - self.final_repeats;
            self.final_repeats -= 1;
            let total = to_units(self.tele.duration, self.rtp_time.frequency());
            let not_before = self.wallclock + self.tele.duration + UPDATE_INTERVAL * repeat;
            let packet = self.packet(total - self.segment, true, not_before);
            return Some(packet);
        }
        if self.done {
            return None;
        }

        let elapsed = self.next_update.min(self.tele.duration);
        let not_before = self.wallclock + elapsed;
        let total = to_units(elapsed, self.rtp_time.frequency());

        // Emit a full segment before the packet for the next segment at this update time.
        if total - self.segment > MAX_SEGMENT {
            let packet = self.packet(MAX_SEGMENT, false, not_before);
            self.segment += MAX_SEGMENT;
            return Some(packet);
        }

        let end = elapsed == self.tele.duration;
        let packet = self.packet(total - self.segment, end, not_before);
        if end {
            self.done = true;
            self.final_repeats = 2;
        } else {
            self.next_update += UPDATE_INTERVAL;
        }
        Some(packet)
    }
}

fn to_units(duration: Duration, clock_rate: Frequency) -> u64 {
    MediaTime::from(duration).rebase(clock_rate).numer()
}

#[cfg(test)]
mod test {
    use super::*;

    fn packets(duration: Duration, clock_rate: Frequency) -> Vec<ToPayload> {
        TelephonePackets::new(
            101.into(),
            None,
            TelephoneEvent {
                event: 5,
                end: true,
                volume: 10,
                duration,
            },
            Instant::now(),
            MediaTime::new(1000, clock_rate),
            ExtensionValues::default(),
        )
        .collect()
    }

    #[test]
    fn telephone_packets_have_separate_send_times() {
        let packets = packets(Duration::from_millis(110), Frequency::EIGHT_KHZ);
        assert_eq!(packets.len(), 8);
        let first = packets[0].not_before.unwrap();
        let offsets: Vec<_> = packets
            .iter()
            .map(|p| p.not_before.unwrap() - first)
            .collect();
        assert_eq!(
            offsets,
            [0, 20, 40, 60, 80, 90, 110, 130].map(Duration::from_millis)
        );
        assert!(packets[0].start_of_talk_spurt);
        assert!(packets[1..].iter().all(|p| !p.start_of_talk_spurt));
        assert!(packets.iter().all(|p| p.rtp_time.numer() == 1000));
        for (index, packet) in packets.iter().enumerate() {
            let tele = TelephoneEvent::parse(&packet.data, Frequency::EIGHT_KHZ).unwrap();
            assert_eq!(tele.end, index >= 5);
        }
    }

    #[test]
    fn long_event_uses_contiguous_segments() {
        let packets = packets(Duration::from_secs(3), Frequency::FORTY_EIGHT_KHZ);
        assert_eq!(packets.len(), 154);
        let at_boundary: Vec<_> = packets
            .iter()
            .filter(|p| {
                p.not_before.unwrap() - packets[0].not_before.unwrap()
                    == Duration::from_millis(1360)
            })
            .collect();
        assert_eq!(at_boundary.len(), 2);
        assert_eq!(at_boundary[0].rtp_time.numer(), 1000);
        assert_eq!(at_boundary[1].rtp_time.numer(), 1000 + MAX_SEGMENT);
        assert!(packets[151..].iter().all(|p| {
            TelephoneEvent::parse(&p.data, Frequency::FORTY_EIGHT_KHZ)
                .unwrap()
                .end
        }));
    }
}
