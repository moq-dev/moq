use std::time::Instant;

use str0m::{Output, Rtc, RtcError};

/// Regression test for a panic when SCTP was started and attempted to transmit data before DTLS was started.
#[test]
pub fn sctp_without_dtls() -> Result<(), RtcError> {
    let mut rtc = Rtc::new(Instant::now());

    rtc.direct_api().start_sctp(true);
    loop {
        // Internally, SCTP is polled for output and returns a `Transmit`, which is passed to DTLS.
        if matches!(rtc.poll_output()?, Output::Timeout(_)) {
            break;
        }
    }

    Ok(())
}
