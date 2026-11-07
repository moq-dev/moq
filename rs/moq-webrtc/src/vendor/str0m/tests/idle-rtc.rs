use std::time::{Duration, Instant};

use is::IceConnectionState;
use str0m::{Event, Input, Output, Rtc, RtcError};

/// Create an RTC instance, but only update its time, without feeding it any input.
/// It should not create any output, busy-loop, or error out.
#[test]
pub fn idle_rtc_instance() -> Result<(), RtcError> {
    const ITERATIONS: usize = 50;
    const SPIN_THRESHOLD: usize = 2;
    const MAX_ADVANCE: Duration = Duration::from_secs(60 * 10);

    let mut next_time = Instant::now();
    let mut spin_count = 0;
    let mut rtc = Rtc::new(next_time);

    for i in 1..(ITERATIONS + 1) {
        rtc.handle_input(Input::Timeout(next_time))?;
        'poll_loop: loop {
            match rtc.poll_output()? {
                Output::Timeout(t) => {
                    if t <= next_time {
                        spin_count += 1;
                        if spin_count >= SPIN_THRESHOLD {
                            panic!(
                                "Rtc instance is spinning since iteration {}",
                                i - (SPIN_THRESHOLD - 1)
                            );
                        }
                    } else {
                        spin_count = 0;
                    }

                    // Don't advance time too far.
                    // We're especially mindful of the `not_happening()` timeout, although we can't
                    // access that from this test.
                    let to_next = t.saturating_duration_since(next_time);
                    next_time = if to_next > MAX_ADVANCE {
                        next_time + MAX_ADVANCE
                    } else {
                        t
                    };

                    break 'poll_loop;
                }
                Output::Event(Event::IceConnectionStateChange(IceConnectionState::Checking)) => {
                    // We'll allow this one.
                }
                output => panic!("Unexpected output {output:?} at iteration {i}"),
            }
        }
    }

    Ok(())
}
