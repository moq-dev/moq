use std::time::{Duration, Instant};
use str0m::media::{MediaKind, Mid};
use str0m::{Input, Output, Reason, Rtc, RtcConfig};

fn next_timeout(rtc: &mut Rtc) -> (Instant, Reason) {
    for _ in 0..32 {
        if let Output::Timeout(deadline) = rtc.poll_output().unwrap() {
            return (deadline, rtc.last_timeout_reason());
        }
    }
    panic!("str0m did not finish draining outputs within 32 polls");
}

fn receiver(ssrc: u32, now: Instant) -> Rtc {
    let mut rtc = RtcConfig::new()
        .set_rtp_mode(true)
        .set_stats_interval(None)
        .set_rtcp_report_interval_audio(Duration::from_secs(1))
        .set_rtcp_report_interval_video(Duration::from_secs(1))
        .build(now);

    let mid: Mid = "audio".into();
    let mut direct = rtc.direct_api();
    direct.declare_media(mid, MediaKind::Audio);
    direct
        .expect_stream_rx(ssrc.into(), None, mid, None)
        .suppress_nack(true);
    rtc
}

#[test]
fn ordinary_receiver_advances_feedback_deadline() {
    let now = Instant::now();
    let mut rtc = receiver(1234, now);
    let expected = now + Duration::from_secs(1);
    let mut timeout = now;
    for _ in 0..32 {
        rtc.handle_input(Input::Timeout(timeout)).unwrap();
        let (deadline, reason) = next_timeout(&mut rtc);
        if reason == Reason::Feedback {
            assert_eq!(deadline, expected);
            return;
        }
        assert!(deadline <= expected, "feedback was not the next deadline");
        timeout = deadline;
    }
    panic!("ordinary receiver did not schedule feedback within 32 timeouts");
}

#[test]
fn ssrc_zero_probe_does_not_rearm_immediate_feedback_forever() {
    let now = Instant::now();
    let mut rtc = receiver(0, now);
    let mut due_timeouts = Vec::new();
    for _ in 0..3 {
        rtc.handle_input(Input::Timeout(now)).unwrap();
        let (deadline, reason) = next_timeout(&mut rtc);
        if deadline > now {
            return;
        }
        due_timeouts.push((deadline, reason));
    }

    panic!("SSRC 0 never advances its timer after three timeout deliveries: {due_timeouts:?}");
}
