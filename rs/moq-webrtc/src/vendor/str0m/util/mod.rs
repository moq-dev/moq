use std::time::{Duration, Instant};

mod bit_pattern;

pub(crate) use bit_pattern::BitPattern;

pub(crate) mod value_history;

mod time_tricks;
pub(crate) use time_tricks::{InstantExt, SystemTimeExt, not_happening};
pub(crate) use time_tricks::{already_happened, epoch_to_beginning};

mod average;
pub(crate) use average::MovingAverage;

pub(crate) trait Soonest {
    fn soonest(self, other: Self) -> Self;
}

impl<T: Default> Soonest for (Option<Instant>, T) {
    fn soonest(self, other: Self) -> Self {
        match (self, other) {
            ((Some(v1), s1), (Some(v2), s2)) => {
                if v1 < v2 {
                    (Some(v1), s1)
                } else {
                    (Some(v2), s2)
                }
            }
            ((None, _), (None, _)) => (None, T::default()),
            ((None, _), (v, s)) => (v, s),
            ((v, s), (None, _)) => (v, s),
        }
    }
}

/// Calculate the round trip time for a given peer as described in
/// [RFC3550 6.4.1](https://datatracker.ietf.org/doc/html/rfc3550#section-6.4.1).
///
/// ## Params
/// - `ntp_time` the offset since 1900-01-01.
/// - `delay` the delay(`DLSR`) since last sender report expressed as fractions of a second in 32 bits.
/// - `last_report` the middle 32 bits of an NTP timestamp for the most recent sender report(LSR)
///   or Receiver Report(LRR).
pub(crate) fn calculate_rtt(ntp_time: Duration, delay: u32, last_report: u32) -> Option<Duration> {
    // [10 Nov 1995 11:33:25.125 UTC]       [10 Nov 1995 11:33:36.5 UTC]
    // n                 SR(n)              A=b710:8000 (46864.500 s)
    // ---------------------------------------------------------------->
    //                    v                 ^
    // ntp_sec =0xb44db705 v               ^ dlsr=0x0005:4000 (    5.250s)
    // ntp_frac=0x20000000  v             ^  lsr =0xb705:2000 (46853.125s)
    //   (3024992005.125 s)  v           ^
    // r                      v         ^ RR(n)
    // ---------------------------------------------------------------->
    //                        |<-DLSR->|
    //                         (5.250 s)
    //
    // A     0xb710:8000 (46864.500 s)
    // DLSR -0x0005:4000 (    5.250 s)
    // LSR  -0xb705:2000 (46853.125 s)
    // -------------------------------
    // delay 0x0006:2000 (    6.125 s)

    // - we want the current middle 32 bits of an NTP timestamp for the current time.
    // We treat the seconds separately to the fractions.
    // [32 bit seconds].[32 bit fractions]
    //         [16 bit].[16 bit]

    // As per RFC delay is 0 in case no SR packet has been received yet.
    if delay == 0 {
        return None;
    }

    let now_secs = ntp_time.as_secs();
    let now_fract_ns = ntp_time.subsec_nanos() as u64;
    let now_fract = ((now_fract_ns * u32::MAX as u64) / 1_000_000_000) as u32;

    // Combine the final 2x16 bits together.
    let now = (now_secs as u32) << 16 | (now_fract >> 16);

    let rtt = now.checked_sub(delay)?.checked_sub(last_report)?;
    let rtt_seconds = rtt >> 16;
    let rtt_fraction = (rtt & (u16::MAX as u32)) as f32 / (u16::MAX as u32) as f32;

    let nanos = (rtt_fraction * 1_000_000_000.0) as u32;
    Some(Duration::new(rtt_seconds as u64, nanos))
}
