use std::mem;
use std::time::{Duration, Instant};

use crate::rtp_::TwccSeq;

use super::super::AckedPacket;
use super::super::time::{TimeDelta, Timestamp};

const BURST_TIME_INTERVAL: Duration = Duration::from_millis(5);
const SEND_TIME_GROUP_LENGTH: Duration = Duration::from_millis(5);
const MAX_BURST_DURATION: Duration = Duration::from_millis(100);

#[derive(Debug, Default)]
pub struct ArrivalGroup {
    first: Option<(TwccSeq, Instant, Instant)>,
    last_seq_no: Option<TwccSeq>,
    last_local_send_time: Option<Instant>,
    last_remote_recv_time: Option<Instant>,
    size: usize,
}

impl ArrivalGroup {
    /// Maybe add a packet to the group.
    ///
    /// Returns [`true`] if a new group needs to be created and [`false`] otherwise.
    fn add_packet(&mut self, packet: &AckedPacket) -> bool {
        match self.belongs_to_group(packet) {
            Belongs::NewGroup => return true,
            Belongs::Skipped => return false,
            Belongs::Yes => {}
        }

        if self.first.is_none() {
            self.first = Some((
                packet.seq_no,
                packet.local_send_time,
                packet.remote_recv_time,
            ));
        }

        self.last_remote_recv_time = self
            .last_remote_recv_time
            .max(Some(packet.remote_recv_time));
        self.last_local_send_time = self.last_local_send_time.max(Some(packet.local_send_time));
        self.size += 1;
        self.last_seq_no = self.last_seq_no.max(Some(packet.seq_no));

        false
    }

    fn belongs_to_group(&self, packet: &AckedPacket) -> Belongs {
        let Some((_, first_local_send_time, first_remote_recv_time)) = self.first else {
            // Start of the group
            return Belongs::Yes;
        };

        let Some(first_send_delta) = packet
            .local_send_time
            .checked_duration_since(first_local_send_time)
        else {
            // Out of order
            return Belongs::Skipped;
        };

        let send_time_delta = Timestamp::from(packet.local_send_time) - self.local_send_time();
        if send_time_delta == TimeDelta::ZERO {
            return Belongs::Yes;
        }
        let arrival_time_delta = Timestamp::from(packet.remote_recv_time) - self.remote_recv_time();

        let propagation_delta = arrival_time_delta - send_time_delta;
        if propagation_delta < TimeDelta::ZERO
            && arrival_time_delta <= BURST_TIME_INTERVAL
            && packet.remote_recv_time - first_remote_recv_time < MAX_BURST_DURATION
        {
            Belongs::Yes
        } else if first_send_delta > SEND_TIME_GROUP_LENGTH {
            Belongs::NewGroup
        } else {
            Belongs::Yes
        }
    }

    /// Calculate the send time delta between self and a subsequent group.
    fn departure_delta(&self, other: &Self) -> TimeDelta {
        Timestamp::from(other.local_send_time()) - self.local_send_time()
    }

    /// Calculate the remote receive time delta between self and a subsequent group.
    fn arrival_delta(&self, other: &Self) -> TimeDelta {
        Timestamp::from(other.remote_recv_time()) - self.remote_recv_time()
    }

    /// The local send time i.e. departure time, for the group.
    ///
    /// Panics if the group doesn't have at least one packet.
    fn local_send_time(&self) -> Instant {
        self.last_local_send_time
            .expect("local_send_time to only be called on non-empty groups")
    }

    /// The remote receive time i.e. arrival time, for the group.
    ///
    /// Panics if the group doesn't have at least one packet.
    fn remote_recv_time(&self) -> Instant {
        self.last_remote_recv_time
            .expect("remote_recv_time to only be called on non-empty groups")
    }
}

/// Whether a given packet is belongs to a group or not.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Belongs {
    /// The packet is belongs to the group.
    Yes,
    /// The packet is does not belong to the group, a new group should be created.
    NewGroup,
    /// The packet was skipped and a decision wasn't made.
    Skipped,
}

impl Belongs {
    #[cfg(test)]
    fn new_group(&self) -> bool {
        matches!(self, Self::NewGroup)
    }
}

#[derive(Debug, Default)]
pub struct ArrivalGroupAccumulator {
    send_time_origin: Option<Instant>,
    previous_group: Option<ArrivalGroup>,
    current_group: ArrivalGroup,
}

impl ArrivalGroupAccumulator {
    ///
    /// Accumulate a packet.
    ///
    /// If adding this packet produced a new delay delta it is returned.
    pub fn accumulate_packet(&mut self, packet: &AckedPacket) -> Option<InterGroupDelayDelta> {
        // Channel outages can queue packets and release them in a compressed burst. Tiny
        // send-time differences can split that burst into extra delay observations and
        // contribute to false overuse detections. Millisecond send times let the zero-delta
        // rule in belongs_to_group keep packets sent together in the same group.
        // GCC's motivation for burst grouping:
        // https://datatracker.ietf.org/doc/html/draft-ietf-rmcat-gcc-02#section-5.2
        //
        // Match libwebrtc's nearest-millisecond precision, including rounding half up:
        // Clock::TimeInMilliseconds -> Timestamp::ms -> UnitBase::ToFraction uses
        // DivideRoundToNearest. See rtc_base/units/unit_base.h:152 in this pinned revision:
        // https://webrtc.googlesource.com/src/+/bef233bc69712a4976442f980b1355b990c1107b
        // Use one origin across groups so rounding boundaries stay fixed. Our origin
        // differs from WebRTC's clock epoch, so individual group boundaries may differ.
        let origin = *self.send_time_origin.get_or_insert(packet.local_send_time);
        let elapsed = packet.local_send_time.checked_duration_since(origin)?;
        let rounded_ms = (elapsed.as_micros() + 500) / 1000;
        let packet = AckedPacket {
            local_send_time: origin + Duration::from_millis(rounded_ms as u64),
            ..*packet
        };

        let need_new_group = self.current_group.add_packet(&packet);

        if !need_new_group {
            return None;
        }

        // Variation between previous group and current.
        let arrival_delta = self.arrival_delta();
        let send_delta = self.send_delta();
        let last_remote_recv_time = self.current_group.remote_recv_time();

        let current_group = mem::take(&mut self.current_group);
        self.previous_group = Some(current_group);

        self.current_group.add_packet(&packet);

        Some(InterGroupDelayDelta {
            send_delta: send_delta?,
            arrival_delta: arrival_delta?,
            last_remote_recv_time,
        })
    }

    fn arrival_delta(&self) -> Option<TimeDelta> {
        self.previous_group
            .as_ref()
            .map(|prev| prev.arrival_delta(&self.current_group))
    }

    fn send_delta(&self) -> Option<TimeDelta> {
        self.previous_group
            .as_ref()
            .map(|prev| prev.departure_delta(&self.current_group))
    }
}

/// The calculate delay delta between two groups of packets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InterGroupDelayDelta {
    /// The delta between the send times of the two groups i.e. delta between the last packet sent
    /// in each group.
    pub send_delta: TimeDelta,
    /// The delta between the remote arrival times of the two groups.
    pub arrival_delta: TimeDelta,
    /// The reported receive time for the last packet in the first arrival group.
    pub last_remote_recv_time: Instant,
}

#[cfg(test)]
mod test {
    use std::time::{Duration, Instant};

    use crate::rtp_::DataSize;

    use super::{AckedPacket, ArrivalGroup, ArrivalGroupAccumulator, Belongs, TimeDelta};

    #[test]
    fn send_timestamps_round_to_nearest_millisecond() {
        let now = Instant::now();
        let mut groups = ArrivalGroupAccumulator::default();
        for (seq, send_us, rounded_ms) in [
            (0, 0, 0),
            (1, 499, 0),
            (2, 500, 1),
            (3, 1499, 1),
            (4, 1500, 2),
        ] {
            groups.accumulate_packet(&AckedPacket {
                seq_no: seq.into(),
                size: DataSize::ZERO,
                local_send_time: now + Duration::from_micros(send_us),
                remote_recv_time: now + Duration::from_micros(send_us),
                local_recv_time: now + Duration::from_millis(100),
            });
            assert_eq!(
                groups.current_group.local_send_time(),
                now + Duration::from_millis(rounded_ms)
            );
        }
    }

    #[test]
    fn compressed_burst_ignores_submillisecond_send_gaps() {
        let now = Instant::now();
        let mut groups = ArrivalGroupAccumulator::default();
        let packet = |seq: u64, send_us, receive_us| AckedPacket {
            seq_no: seq.into(),
            size: DataSize::ZERO,
            local_send_time: now + Duration::from_micros(send_us),
            remote_recv_time: now + Duration::from_micros(receive_us),
            local_recv_time: now + Duration::from_millis(100),
        };

        // The 29 us send gap becomes zero at millisecond precision. The 250 us
        // receive gap must not split the compressed burst into another observation.
        for (seq, send_us, receive_us) in [(0, 0, 0), (1, 6684, 1000), (2, 6713, 1250)] {
            assert!(
                groups
                    .accumulate_packet(&packet(seq, send_us, receive_us))
                    .is_none()
            );
        }
        assert_eq!(groups.current_group.size, 3);

        assert!(
            groups
                .accumulate_packet(&packet(3, 20_000, 20_000))
                .is_none()
        );
        let delta = groups
            .accumulate_packet(&packet(4, 30_000, 30_000))
            .unwrap();
        assert_eq!(delta.send_delta, TimeDelta::from_millis(13));
        assert_eq!(delta.arrival_delta, Duration::from_micros(18_750));
    }

    #[test]
    fn millisecond_send_origin_is_shared_between_groups() {
        let now = Instant::now();
        let mut groups = ArrivalGroupAccumulator::default();
        let mut delta = None;

        for (seq, send_us, receive_us) in [
            (0, 0, 0),
            (1, 20_490, 12_000),
            (2, 26_499, 13_000),
            (3, 26_501, 14_250),
        ] {
            delta = groups.accumulate_packet(&AckedPacket {
                seq_no: seq.into(),
                size: DataSize::ZERO,
                local_send_time: now + Duration::from_micros(send_us),
                remote_recv_time: now + Duration::from_micros(receive_us),
                local_recv_time: now + Duration::from_millis(100),
            });
        }

        let delta = delta.unwrap();
        assert_eq!(delta.send_delta, TimeDelta::from_millis(26));
        assert_eq!(delta.arrival_delta, TimeDelta::from_millis(13));
    }

    #[test]
    fn test_arrival_group_all_packets_belong_to_empty_group() {
        let now = Instant::now();
        let group = ArrivalGroup::default();

        assert_eq!(
            group.belongs_to_group(&AckedPacket {
                seq_no: 1.into(),
                size: DataSize::ZERO,
                local_send_time: now,
                remote_recv_time: now + duration_us(10),
                local_recv_time: now + duration_us(12),
            }),
            Belongs::Yes,
            "Any packet should belong to an empty arrival group"
        );
    }

    #[test]
    fn test_arrival_group_all_packets_sent_within_burst_interval_belong() {
        let now = Instant::now();
        #[allow(clippy::vec_init_then_push)]
        let packets = {
            let mut packets = vec![];

            packets.push(AckedPacket {
                seq_no: 0.into(),
                size: DataSize::ZERO,
                local_send_time: now,
                remote_recv_time: now + duration_us(150),
                local_recv_time: now + duration_us(200),
            });

            packets.push(AckedPacket {
                seq_no: 1.into(),
                size: DataSize::ZERO,
                local_send_time: now + duration_us(50),
                remote_recv_time: now + duration_us(225),
                local_recv_time: now + duration_us(275),
            });

            packets.push(AckedPacket {
                seq_no: 2.into(),
                size: DataSize::ZERO,
                local_send_time: now + duration_us(1005),
                remote_recv_time: now + duration_us(1140),
                local_recv_time: now + duration_us(1190),
            });

            packets.push(AckedPacket {
                seq_no: 3.into(),
                size: DataSize::ZERO,
                local_send_time: now + duration_us(4995),
                remote_recv_time: now + duration_us(5001),
                local_recv_time: now + duration_us(5051),
            });

            // Should not belong
            packets.push(AckedPacket {
                seq_no: 4.into(),
                size: DataSize::ZERO,
                local_send_time: now + duration_us(5700),
                remote_recv_time: now + duration_us(6000),
                local_recv_time: now + duration_us(5750),
            });

            packets
        };

        let mut group = ArrivalGroup::default();

        for p in packets {
            let need_new_group = group.belongs_to_group(&p).new_group();
            if !need_new_group {
                group.add_packet(&p);
            }
        }

        assert_eq!(group.size, 4, "Expected group to contain 4 packets");
    }

    #[test]
    fn test_arrival_group_out_order_arrival_ignored() {
        let now = Instant::now();
        #[allow(clippy::vec_init_then_push)]
        let packets = {
            let mut packets = vec![];

            packets.push(AckedPacket {
                seq_no: 0.into(),
                size: DataSize::ZERO,
                local_send_time: now,
                remote_recv_time: now + duration_us(150),
                local_recv_time: now + duration_us(200),
            });

            packets.push(AckedPacket {
                seq_no: 1.into(),
                size: DataSize::ZERO,
                local_send_time: now + duration_us(50),
                remote_recv_time: now + duration_us(225),
                local_recv_time: now + duration_us(275),
            });

            packets.push(AckedPacket {
                seq_no: 2.into(),
                size: DataSize::ZERO,
                local_send_time: now + duration_us(1005),
                remote_recv_time: now + duration_us(1140),
                local_recv_time: now + duration_us(1190),
            });

            packets.push(AckedPacket {
                seq_no: 3.into(),
                size: DataSize::ZERO,
                local_send_time: now + duration_us(4995),
                remote_recv_time: now + duration_us(5001),
                local_recv_time: now + duration_us(5051),
            });

            // Should be skipped
            packets.push(AckedPacket {
                seq_no: 4.into(),
                size: DataSize::ZERO,
                local_send_time: now - duration_us(100),
                remote_recv_time: now + duration_us(5000),
                local_recv_time: now + duration_us(5050),
            });

            // Should not belong
            packets.push(AckedPacket {
                seq_no: 5.into(),
                size: DataSize::ZERO,
                local_send_time: now + duration_us(5700),
                remote_recv_time: now + duration_us(6000),
                local_recv_time: now + duration_us(6050),
            });

            packets
        };

        let mut group = ArrivalGroup::default();

        for p in packets {
            let need_new_group = group.belongs_to_group(&p).new_group();
            if !need_new_group {
                group.add_packet(&p);
            }
        }

        assert_eq!(group.size, 4, "Expected group to contain 4 packets");
    }

    #[test]
    fn test_arrival_group_arrival_membership() {
        let now = Instant::now();
        #[allow(clippy::vec_init_then_push)]
        let packets = {
            let mut packets = vec![];

            packets.push(AckedPacket {
                seq_no: 0.into(),
                size: DataSize::ZERO,
                local_send_time: now,
                remote_recv_time: now + duration_us(150),
                local_recv_time: now + duration_us(200),
            });

            packets.push(AckedPacket {
                seq_no: 1.into(),
                size: DataSize::ZERO,
                local_send_time: now + duration_us(50),
                remote_recv_time: now + duration_us(225),
                local_recv_time: now + duration_us(275),
            });

            packets.push(AckedPacket {
                seq_no: 2.into(),
                size: DataSize::ZERO,
                local_send_time: now + duration_us(5152),
                // Just less than 5ms inter arrival delta
                remote_recv_time: now + duration_us(5224),
                local_recv_time: now + duration_us(5274),
            });

            // Should not belong
            packets.push(AckedPacket {
                seq_no: 3.into(),
                size: DataSize::ZERO,
                local_send_time: now + duration_us(5700),
                remote_recv_time: now + duration_us(6000),
                local_recv_time: now + duration_us(6050),
            });

            packets
        };

        let mut group = ArrivalGroup::default();

        for p in packets {
            let need_new_group = group.belongs_to_group(&p).new_group();
            if !need_new_group {
                group.add_packet(&p);
            }
        }

        assert_eq!(group.size, 3, "Expected group to contain 4 packets");
    }

    #[test]
    fn group_reorder() {
        let data = vec![
            ((Duration::from_millis(0), Duration::from_millis(0)), None),
            ((Duration::from_millis(60), Duration::from_millis(5)), None),
            ((Duration::from_millis(40), Duration::from_millis(10)), None),
            (
                (Duration::from_millis(70), Duration::from_millis(20)),
                Some((TimeDelta::from_millis(-20), TimeDelta::from_millis(5))),
            ),
        ];

        let now = Instant::now();
        let mut aga = ArrivalGroupAccumulator::default();

        for ((local_send_time, remote_recv_time), deltas) in data {
            let group_delta = aga.accumulate_packet(&AckedPacket {
                seq_no: Default::default(),
                size: Default::default(),
                local_send_time: now + local_send_time,
                remote_recv_time: now + remote_recv_time,
                local_recv_time: Instant::now(), // does not matter
            });

            assert_eq!(group_delta.map(|d| (d.send_delta, d.arrival_delta)), deltas);
        }
    }

    fn duration_us(us: u64) -> Duration {
        Duration::from_micros(us)
    }
}
