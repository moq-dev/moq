use std::backtrace::Backtrace;
use std::sync::LazyLock;
use std::time::{Duration, Instant, SystemTime};

pub(crate) fn not_happening() -> Instant {
    const YEARS_100: Duration = Duration::from_secs(60 * 60 * 24 * 365 * 100);
    static FUTURE: LazyLock<Instant> = LazyLock::new(|| Instant::now() + YEARS_100);
    *FUTURE
}

// The goal here is to make a constant "beginning of time" in both Instant and SystemTime
// that we can use as relative values for the rest of str0m.
// This is indeed a bit dodgy, but we want str0m's internal idea of time to be completely
// driven from the external API using `Instant`. What works against us is that Instant can't
// represent things like UNIX EPOCH (but SystemTime can).
static BEGINNING_OF_TIME: LazyLock<(Instant, SystemTime)> = LazyLock::new(|| {
    // These two should be "frozen" the same instant. Hopefully they are not differing too much.
    let now = Instant::now();
    let now_sys = SystemTime::now();

    // Find an Instant in the past which is up to an hour back.
    let beginning_of_time = {
        let mut secs = 3600;
        loop {
            let dur = Duration::from_secs(secs);
            if let Some(v) = now.checked_sub(dur) {
                break v;
            }
            secs -= 1;
            if secs == 0 {
                panic!("Failed to find a beginning of time instant");
            }
        }
    };

    // This might be less than 1 hour if the machine uptime is less.
    let since_beginning_of_time = Instant::now() - beginning_of_time;

    let beginning_of_time_sys = now_sys - since_beginning_of_time;

    // This pair represents our "beginning of time" for the same moment.
    (beginning_of_time, beginning_of_time_sys)
});

pub fn epoch_to_beginning() -> Duration {
    BEGINNING_OF_TIME
        .1
        .duration_since(SystemTime::UNIX_EPOCH)
        .expect("beginning of time to be after epoch")
}

pub(crate) fn already_happened() -> Instant {
    BEGINNING_OF_TIME.0
}

pub trait InstantExt {
    /// Convert an Instant to a Duration for unix time.
    ///
    /// First ever time must be "now".
    ///
    /// panics if `time` goes backwards, i.e. we use this for one Instant and then an earlier Instant.
    fn to_unix_duration(&self) -> Duration;

    /// Convert an Instant to a Duration for ntp time.
    fn to_ntp_duration(&self) -> Duration;

    /// Convert an Instant to a SystemTime for ntp time
    fn to_system_time(&self) -> SystemTime;
}

pub trait SystemTimeExt {
    /// Convert an ntp_64 as seen in SR to a SystemTime.
    fn from_ntp_64(v: u64) -> SystemTime;

    /// Convert a SystemTime to ntp_64.
    fn as_ntp_64(&self) -> u64;
}

// RTP spec "wallclock" uses NTP time, which starts at 1900-01-01.
//
// https://tools.ietf.org/html/rfc868
//
// 365 days * 70 years + 17 leap year days
// (365 * 70 + 17) * 86400 = 2208988800
const SECS_1900: u64 = 2_208_988_800;
const MICROS_1900: u64 = SECS_1900 * 1_000_000;

/// 2^32 as float.
const F32: f64 = 4_294_967_296.0;

impl InstantExt for Instant {
    fn to_unix_duration(&self) -> Duration {
        // This is a bit fishy. We "freeze" a moment in time for Instant and SystemTime,
        // so we can make relative comparisons of Instant - Instant and translate that to
        // SystemTime - unix epoch. Hopefully the error is quite small.
        if *self < BEGINNING_OF_TIME.0 {
            warn!("{}", Backtrace::force_capture());
            warn!("Time went backwards from beginning_of_time Instant");
        }

        let duration_since_time_0 = self.duration_since(BEGINNING_OF_TIME.0);
        let system_time = BEGINNING_OF_TIME.1 + duration_since_time_0;

        system_time
            .duration_since(SystemTime::UNIX_EPOCH)
            .expect("clock to go forwards from unix epoch")
    }

    fn to_ntp_duration(&self) -> Duration {
        self.to_unix_duration() + Duration::from_micros(MICROS_1900)
    }

    fn to_system_time(&self) -> SystemTime {
        // This is a bit fishy. We "freeze" a moment in time for Instant and SystemTime,
        // so we can make relative comparisons of Instant - Instant and translate that to
        // SystemTime - unix epoch. Hopefully the error is quite small.
        if *self < BEGINNING_OF_TIME.0 {
            warn!("{}", Backtrace::force_capture());
            warn!("Time went backwards from beginning_of_time Instant");
        }

        let duration_since_time_0 = self.duration_since(BEGINNING_OF_TIME.0);
        BEGINNING_OF_TIME.1 + duration_since_time_0
    }
}

impl SystemTimeExt for SystemTime {
    fn from_ntp_64(v: u64) -> SystemTime {
        // https://tools.ietf.org/html/rfc3550#section-4
        // Wallclock time (absolute date and time) is represented using the
        // timestamp format of the Network Time Protocol (NTP), which is in
        // seconds relative to 0h UTC on 1 January 1900 [4]. The full
        // resolution NTP timestamp is a 64-bit unsigned fixed-point number with
        // the integer part in the first 32 bits and the fractional part in the
        // last 32 bits.
        let secs_ntp = (v as f64) / F32;

        // Shift to UNIX EPOCH
        let secs_epoch = secs_ntp - SECS_1900 as f64;

        // Duration not allowed to be negative
        let secs_dur = Duration::try_from_secs_f64(secs_epoch).unwrap_or(Duration::ZERO);

        // Time in SystemTime
        SystemTime::UNIX_EPOCH + secs_dur
    }

    fn as_ntp_64(&self) -> u64 {
        let secs_epoch = if let Ok(value) = self.duration_since(SystemTime::UNIX_EPOCH) {
            value.as_secs_f64() + SECS_1900 as f64
        } else {
            0.0
        };

        (secs_epoch * F32) as u64
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn not_happening_works() {
        assert_eq!(not_happening(), not_happening());
        assert!(Instant::now() < not_happening());
    }

    #[test]
    fn already_happened_works() {
        assert_eq!(already_happened(), already_happened());
        assert!(Instant::now() > already_happened());
    }

    #[test]
    fn already_happened_ne() {
        assert_ne!(not_happening(), already_happened())
    }

    #[test]
    fn ntp_64_from_to() {
        let now = SystemTime::now();
        let ntp = now.as_ntp_64();
        let now2 = SystemTime::from_ntp_64(ntp);
        let abs = if now > now2 {
            now.duration_since(now2)
        } else {
            now2.duration_since(now)
        };
        assert!(abs.unwrap() < Duration::from_millis(1));
    }

    #[test]
    fn from_ntp_64_zero() {
        let s = SystemTime::from_ntp_64(0);
        assert_eq!(
            s.duration_since(SystemTime::UNIX_EPOCH).unwrap(),
            Duration::ZERO
        );
    }

    #[test]
    fn from_ntp_64_val1() {
        let s = SystemTime::from_ntp_64((3971792775u64 << 32) | 4184015405u64);
        match s.duration_since(SystemTime::UNIX_EPOCH) {
            Ok(n) => assert_eq!(n.as_micros(), 1762803975974166),
            Err(_) => panic!("Cannot calculate unix epoch"),
        }
    }
}
