use std::time::SystemTime;
use std::time::{Duration, Instant};

use once_cell::sync::Lazy;

// The goal here is to make a constant "beginning of time" in both Instant and SystemTime
// that we can use as relative values for the rest of dimpl.
// This is indeed a bit dodgy, but we want dimpl's internal idea of time to be completely
// driven from the external API using `Instant`. What works against us is that Instant can't
// represent things like UNIX EPOCH (but SystemTime can).
static BEGINNING_OF_TIME: Lazy<(Instant, SystemTime)> = Lazy::new(|| {
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

pub trait InstantExt {
    /// Convert an Instant to a Duration for unix time.
    ///
    /// First ever time must be "now".
    ///
    /// panics if `time` goes backwards, i.e. we use this for one Instant and then an earlier Instant.
    fn to_unix_duration(&self) -> Duration;
}

impl InstantExt for Instant {
    fn to_unix_duration(&self) -> Duration {
        // This is a bit fishy. We "freeze" a moment in time for Instant and SystemTime,
        // so we can make relative comparisons of Instant - Instant and translate that to
        // SystemTime - unix epoch. Hopefully the error is quite small.
        if *self < BEGINNING_OF_TIME.0 {
            warn!("Time went backwards from beginning_of_time Instant");
        }

        let duration_since_time_0 = self.duration_since(BEGINNING_OF_TIME.0);
        let system_time = BEGINNING_OF_TIME.1 + duration_since_time_0;

        system_time
            .duration_since(SystemTime::UNIX_EPOCH)
            .expect("clock to go forwards from unix epoch")
    }
}
