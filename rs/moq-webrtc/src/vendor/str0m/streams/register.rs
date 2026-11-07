use std::time::{Duration, Instant};

use crate::rtp_::{Nack, ReceptionReport, SeqNo};

use super::register_nack::NackRegister;

#[derive(Debug)]
pub struct ReceiverRegister {
    nack: NackRegister,

    /// First sequence number received
    first: Option<SeqNo>,

    /// Number of packets received
    count: u64,

    /// Previously received time point.
    time_point_prior: Option<TimePoint>,

    /// Expected at last reception report generation.
    expected_prior: i64,

    /// Received at last reception report generation.
    received_prior: i64,

    /// Interarrival jitter in **microseconds**.
    ///
    /// RTCP carries jitter in **RTP timestamp units** so use
    /// [`ReceiverRegister::jitter_in_rtp_ts`] for the wire value.
    jitter: f32,
}

#[derive(Debug, Clone, Copy)]
struct TimePoint {
    arrival: Instant,
    rtp_time: u32,
    clock_rate: u32,
}

impl TimePoint {
    fn is_same(&self, other: TimePoint) -> bool {
        self.rtp_time == other.rtp_time
    }

    fn delta(&self, other: TimePoint) -> f32 {
        // See
        // https://www.rfc-editor.org/rfc/rfc3550#appendix-A.8
        //
        // rdur is often i 90kHz (for video) or 48kHz (for audio). we need
        // a time unit of Duration, that is likely to give us an increase between
        // 1 in rdur. milliseconds is thus "too coarse"

        // wrapping_sub to handle RTP time rollover
        let rtp_diff = self.rtp_time.wrapping_sub(other.rtp_time) as i32;
        let rdur = rtp_diff as f32 * 1_000_000.0 / self.clock_rate as f32;

        let tdur = (self.arrival - other.arrival).as_micros() as f32;

        let d = (tdur - rdur).abs();

        trace!("Timepoint delta: {}", d);

        d
    }
}

impl ReceiverRegister {
    pub fn new(max_seq_no: Option<SeqNo>) -> Self {
        ReceiverRegister {
            nack: NackRegister::new(max_seq_no),
            first: None,
            count: 0,
            time_point_prior: None,
            expected_prior: 0,
            received_prior: 0,
            jitter: 0.0,
        }
    }

    pub fn accepts(&self, seq: SeqNo) -> bool {
        self.nack.accepts(seq)
    }

    /// Mark `seq` as present without counting it as received: it was rebuilt from redundancy
    /// (RFC 2198 RED) rather than arriving on the wire. It will no longer be NACKed or accepted
    /// as new, while `count` (and thus the reception report loss) still reflects wire loss.
    pub fn mark_recovered(&mut self, seq: SeqNo) {
        self.nack.update(seq);
    }

    pub fn update(&mut self, seq: SeqNo, arrival: Instant, rtp_time: u32, clock_rate: u32) -> bool {
        if self.first.is_none() {
            self.first = Some(seq);
        }

        let new = self.nack.update(seq);

        if new {
            self.count += 1;
        }

        self.update_time(arrival, rtp_time, clock_rate);

        new
    }

    pub fn recovery_deadline(
        &self,
        range: std::ops::Range<SeqNo>,
        received: Instant,
        rtt: Option<Duration>,
    ) -> Option<Instant> {
        self.nack.recovery_deadline(range, received, rtt)
    }

    pub fn has_pending_nacks(&self) -> bool {
        self.nack.has_pending_nacks()
    }

    /// Generates a NACK report
    pub fn nack_report(
        &mut self,
        now: Instant,
        rtt: Option<Duration>,
    ) -> Option<impl Iterator<Item = Nack>> {
        self.nack.nack_reports(now, rtt)
    }

    /// Create a new reception report.
    ///
    /// This modifies the state since fraction_lost is calculated
    /// since the last call to this function.
    pub fn reception_report(&mut self) -> Option<ReceptionReport> {
        let first = self.first?;
        let last = self.max_seq()?;

        let expected = expected(first, last);

        Some(ReceptionReport {
            ssrc: 0.into(),
            fraction_lost: self.fraction_lost(expected, self.count as i64),
            packets_lost: packets_lost(expected, self.count as i64),
            max_seq: (*last % ((u32::MAX as u64) + 1_u64)) as u32,
            jitter: self.jitter_in_rtp_ts(),
            last_sr_time: 0,
            last_sr_delay: 0,
        })
    }

    pub fn max_seq(&self) -> Option<SeqNo> {
        self.nack.max_seq()
    }

    pub fn clear(&mut self, max_seq_no: Option<SeqNo>) {
        self.nack = NackRegister::new(max_seq_no);
        self.count = 0;
        self.first = None;
        self.time_point_prior = None;
        self.expected_prior = 0;
        self.received_prior = 0;
        self.jitter = 0.0;
    }

    fn update_time(&mut self, arrival: Instant, rtp_time: u32, clock_rate: u32) {
        let tp = TimePoint {
            arrival,
            rtp_time,
            clock_rate,
        };

        if let Some(prior) = self.time_point_prior {
            if tp.is_same(prior) {
                // rtp_time didn't move forward. this is quite normal
                // when multiple rtp packets are needed for one keyframe.

                // https://www.cs.columbia.edu/~hgs/rtp/faq.html#jitter
                //
                // If several packets, say, within a video frame, bear the
                // same timestamp, it is advisable to only use the first
                // packet in a frame to compute the jitter. (This issue may
                // be addressed in a future version of the specification.)
                // Jitter is computed in timestamp units. For example, for
                // an audio stream sampled at 8,000 Hz, the arrival time
                // measured with the local clock is converted by multiplying
                // the seconds by 8,000.
                //
                // Steve Casner wrote:
                //
                // For encodings such as MPEG that transmit data in a
                // different order than it was sampled, this adds noise
                // into the jitter calculation. I have heard handwavy
                // arguments that this factor can be calculated out given
                // that you know the shape of the noise, but my math
                // isn't strong enough for that.
                //
                // In many of the cases that we care about, the jitter
                // introduced by MPEG will be small enough that when the
                // network jitter is of the same order we don't have a
                // problem anyway.
                //
                // There is another problem for video in that all of the
                // packets of a frame have the same timestamp because the
                // whole frame is sampled at once. However, the
                // dispersion in time of those packets really is all part
                // of the network transfer process that the receiver must
                // accommodate with its buffer.
                //
                // It has been suggested that jitter be calculated only
                // on the first packet of a video frame, or only on "I"
                // frames for MPEG. However, that may color the results
                // also because those packets may see transit delays
                // different than the following packets see.
                //
                // The main point to remember is that the primary
                // function of the RTP timestamp is to represent the
                // inherent notion of real time associated with the
                // media. It also turns out to be useful for the jitter
                // measure, but that is a secondary function.
                //
                // The jitter value is not expected to be useful as an
                // absolute value. It is more useful as a means of
                // comparing the reception quality at two receiver or
                // comparing the reception quality 5 minutes ago to now.

                return;
            }

            // update jitter.
            let d = tp.delta(prior);

            self.jitter += (1.0 / 16.0) * (d - self.jitter);
        }

        self.time_point_prior = Some(tp);
    }

    // Calculations from here
    // https://www.rfc-editor.org/rfc/rfc3550#appendix-A.3

    /// Fraction lost since last call.
    fn fraction_lost(&mut self, expected: i64, received: i64) -> u8 {
        let expected_interval = expected - self.expected_prior;
        self.expected_prior = expected;

        let received_interval = received - self.received_prior;
        self.received_prior = received;

        let lost_interval = expected_interval - received_interval;

        let lost = if expected_interval == 0 || lost_interval <= 0 {
            0
        } else {
            ((lost_interval << 8) / expected_interval).min(255)
        } as u8;

        trace!("Reception fraction lost: {}", lost);

        lost
    }

    /// Jitter in RTP timestamp units.
    fn jitter_in_rtp_ts(&self) -> u32 {
        let Some(sample_rate) = self.time_point_prior.map(|tp| tp.clock_rate) else {
            return 0;
        };
        (self.jitter / 1_000_000.0 * sample_rate as f32).round() as u32
    }
}

/// Absolute number of lost packets.
fn packets_lost(expected: i64, received: i64) -> u32 {
    // Since this signed number is carried in 24 bits, it should be clamped
    // at 0x7fffff for positive loss or 0x800000 for negative loss rather
    // than wrapping around.
    let lost_t = expected - received;
    if lost_t > 0x7fffff {
        0x7fffff_u32
    } else if lost_t < -0x7fffff {
        0x8000000_u32
    } else {
        lost_t as u32
    }
}

fn expected(first: SeqNo, last: SeqNo) -> i64 {
    let delta = (*last - *first) as i64;
    delta.saturating_add(1)
}

#[cfg(test)]
mod test {
    use std::time::{Duration, Instant};

    use crate::streams::register::{ReceiverRegister, expected, packets_lost};

    #[test]
    fn clearing_register_resets_nack_timing() {
        let mut reg = ReceiverRegister::new(None);
        let now = Instant::now();
        reg.update(10.into(), now, 0, 90_000);
        reg.update(12.into(), now, 3000, 90_000);
        assert!(reg.nack_report(now, None).is_some());
        assert!(reg.nack_report(now, None).is_none());
        reg.clear(Some(10.into()));
        assert!(!reg.has_pending_nacks());
        reg.update(12.into(), now, 3000, 90_000);
        let nacks: Vec<_> = reg.nack_report(now, None).expect("fresh loss").collect();
        assert_eq!(nacks[0].reports[0].pid, 11);
    }

    #[test]
    fn jitter_at_0() {
        let mut r = ReceiverRegister::new(None);

        // 100 fps in clock rate 90kHz => 90_000/100 = 900 per frame
        // 1/100 * 1_000_000 = 10_000 microseconds per frame.

        let start = Instant::now();
        let dur = Duration::from_micros(10_000);

        r.update_time(start + 4 * dur, 1234 + 4 * 900, 90_000);
        r.update_time(start + 5 * dur, 1234 + 5 * 900, 90_000);
        r.update_time(start + 6 * dur, 1234 + 6 * 900, 90_000);
        r.update_time(start + 7 * dur, 1234 + 7 * 900, 90_000);
        assert_eq!(r.jitter, 0.0);
    }

    #[test]
    fn jitter_at_20() {
        let mut r = ReceiverRegister::new(None);

        // 100 fps in clock rate 90kHz => 90_000/100 = 900 per frame
        // 1/100 * 1_000_000 = 10_000 microseconds per frame.

        let start = Instant::now();
        let dur = Duration::from_micros(10_000);
        let off = Duration::from_micros(10);

        for i in 4..1000 {
            let arrival = if i % 2 == 0 {
                start + (i * dur).checked_sub(off).unwrap()
            } else {
                start + i * dur + off
            };
            r.update((i as u64).into(), arrival, 1234 + i * 900, 90_000);
        }

        // jitter should converge on 20.0
        assert!(
            (20.0 - r.jitter).abs() < 0.01,
            "Expected jitter to converge at 20.0, jitter was: {}",
            r.jitter
        );

        // jitter is also present in reception report
        let report = r.reception_report().expect("some report");
        // 90kHz is 11.1us ticks, so 20us jitter is 1.8 tick which equals 2
        // after rounding to int.
        assert_eq!(report.jitter, 2);
        assert_eq!(report.jitter, r.jitter_in_rtp_ts());
    }

    #[test]
    fn fraction_lost_is_zero_when_an_interval_receives_more_than_expected() {
        let mut reg = ReceiverRegister::new(None);

        assert_eq!(reg.fraction_lost(120, 118), 4);
        assert_eq!(reg.fraction_lost(240, 240), 0);
    }

    #[test]
    fn fraction_lost_saturates_when_everything_is_lost() {
        let mut reg = ReceiverRegister::new(None);

        assert_eq!(reg.fraction_lost(100, 0), 255);
    }

    #[test]
    fn expected_received_loss() {
        let first = 14.into();
        let last = 17.into();
        let expected = expected(first, last);
        assert_eq!(expected, 4);
        // none of 4 was lost
        assert_eq!(packets_lost(expected, 4), 0);
        // one of 4 was lost:329
        assert_eq!(packets_lost(expected, 3), 1);
    }

    #[test]
    fn expected_overflow() {
        let last = 0x7fff_ffff_ffff_ffff_u64.into();
        let first = 0_u64.into();
        let expected = expected(first, last);
        assert_eq!(expected, i64::MAX);
    }

    #[test]
    fn receiver_report() {
        let mut r = ReceiverRegister::new(None);
        let now = Instant::now();
        let rtp_time = 0;

        // 50 % lost
        for i in 10..14 {
            r.update((i as u64).into(), now, rtp_time, 90_000);
        }
        r.update(19.into(), now, rtp_time, 90_000);

        let report = r.reception_report().expect("some report");
        assert_eq!(128, report.fraction_lost);
        assert_eq!(5, report.packets_lost);
        assert_eq!(19, report.max_seq);
        assert_eq!(0, report.jitter);
    }

    #[test]
    fn simple_jitter_computation() {
        // SimpleJitterComputation from receive_statistics_unittest.cc
        const MS_PER_PACKET: u64 = 20;
        const CODEC_SAMPLE_RATE: u32 = 48_000;
        const SAMPLES_PER_PACKET: u32 = MS_PER_PACKET as u32 * CODEC_SAMPLE_RATE / 1_000;
        const LATE_ARRIVAL_DELTA_MS: u64 = 100;
        const LATE_DELTA_SAMPLES: u32 = LATE_ARRIVAL_DELTA_MS as u32 * CODEC_SAMPLE_RATE / 1_000;

        let mut clock = Instant::now();
        let mut r = ReceiverRegister::new(None);

        r.update_time(clock, 0, CODEC_SAMPLE_RATE);
        clock += Duration::from_millis(MS_PER_PACKET + LATE_ARRIVAL_DELTA_MS);
        r.update_time(clock, SAMPLES_PER_PACKET, CODEC_SAMPLE_RATE);

        assert_eq!(r.jitter_in_rtp_ts(), LATE_DELTA_SAMPLES / 16);
    }

    #[test]
    fn all_packets_have_same_frequency() {
        // AllPacketsHaveSamePayloadTypeFrequency from receive_statistics_unittest.cc
        let mut clock = Instant::now();
        let mut r = ReceiverRegister::new(None);

        r.update_time(clock, 1, 8_000);
        clock += Duration::from_millis(50);
        r.update_time(clock, 1 + 160, 8_000);
        clock += Duration::from_millis(50);
        r.update_time(clock, 1 + 160 + 160, 8_000);

        // packet1: no jitter calculation
        // packet2: jitter = 0[jitter] + (abs(50[receive time ms] *
        //          8[frequency KHz] - 160[timestamp diff]) * 16 - 0[jitter] + 8)
        //          / 16 = 240
        // packet3: jitter = 240[jitter] + (abs(50[receive time ms] *
        //          8[frequency KHz] - 160[timestamp diff]) * 16 - 240[jitter] + 8)
        //          / 16 = 465
        // final jitter: 465 / 16 = 29
        assert_eq!(r.jitter_in_rtp_ts(), 29);
    }

    #[test]
    fn jitter_rtp_timestamp_rollover() {
        // Same as jitter_same_frequency_three_packets but the timestamps are
        // anchored so the u32 boundary falls between packets 1 and 2.

        let rtp_time_1 = u32::MAX - 79;
        let rtp_time_2 = rtp_time_1.wrapping_add(160);
        let rtp_time_3 = rtp_time_2.wrapping_add(160);

        let mut clock = Instant::now();
        let mut r = ReceiverRegister::new(None);

        r.update_time(clock, rtp_time_1, 8_000);
        clock += Duration::from_millis(50);
        r.update_time(clock, rtp_time_2, 8_000);
        clock += Duration::from_millis(50);
        r.update_time(clock, rtp_time_3, 8_000);

        // Same result as jitter_same_frequency_three_packets.
        assert_eq!(r.jitter_in_rtp_ts(), 29);
    }
}
