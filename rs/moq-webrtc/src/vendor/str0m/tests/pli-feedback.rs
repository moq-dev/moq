use std::time::Duration;

use str0m::media::MediaKind;
use str0m::rtp::Ssrc;
use str0m::{Event, RtcError};

mod common;
use common::{connect_l_r, init_crypto_default, init_log, progress};

/// A PLI sent from one peer via [`DirectApi::send_pli_feedback`] is received by the
/// other peer as an `Event::KeyframeRequest`.
#[test]
pub fn pli_feedback_direct_api() -> Result<(), RtcError> {
    init_log();
    init_crypto_default();

    let (mut l, mut r) = connect_l_r();

    // R sources a video stream (ssrc_r) that L receives; L will request a keyframe for it.
    let mid = "vid".into();
    let ssrc_l: Ssrc = 100.into();
    let ssrc_r: Ssrc = 200.into();

    l.direct_api().declare_media(mid, MediaKind::Video);
    l.direct_api().declare_stream_tx(ssrc_l, None, mid, None);
    l.direct_api().expect_stream_rx(ssrc_r, None, mid, None);

    r.direct_api().declare_media(mid, MediaKind::Video);
    r.direct_api().declare_stream_tx(ssrc_r, None, mid, None);
    r.direct_api().expect_stream_rx(ssrc_l, None, mid, None);

    // Sync clocks
    let max = l.last.max(r.last);
    l.last = max;
    r.last = max;

    // L requests a keyframe for R's stream, choosing L's SSRC as the feedback sender.
    l.direct_api().send_pli_feedback(ssrc_l, ssrc_r);

    // Progress until R receives the keyframe request event.
    let deadline = l.last + Duration::from_secs(5);
    let mut received = false;

    loop {
        if l.last > deadline || r.last > deadline {
            break;
        }
        progress(&mut l, &mut r)?;

        for (_time, event) in r.events.drain(..) {
            if let Event::KeyframeRequest(req) = event {
                assert_eq!(req.mid, mid);
                received = true;
            }
        }

        if received {
            break;
        }
    }

    assert!(received, "R should have received a KeyframeRequest from L");

    Ok(())
}
