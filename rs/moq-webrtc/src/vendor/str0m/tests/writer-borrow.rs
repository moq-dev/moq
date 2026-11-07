//! Keep named Writer handles compatible with non-lexical Rtc borrows.

use std::time::Instant;

use str0m::media::{KeyframeRequestKind, MediaKind};
use str0m::{Input, Output, Rtc, RtcError};

mod common;
use common::init_crypto_default;

#[test]
fn named_writer_releases_rtc_after_keyframe_request() -> Result<(), RtcError> {
    init_crypto_default();
    let now = Instant::now();
    let mut rtc = Rtc::new(now);
    let mid = "video".into();
    rtc.direct_api().declare_media(mid, MediaKind::Video);
    rtc.direct_api()
        .expect_stream_rx(42.into(), None, mid, None);
    rtc.handle_input(Input::Timeout(now))?;
    while !matches!(rtc.poll_output()?, Output::Timeout(_)) {}

    let mut writer = rtc.writer(mid).unwrap();
    writer.request_keyframe(None, KeyframeRequestKind::Pli)?;

    // A named writer's last use ends its borrow, so callers can poll immediately
    // without wrapping the request in a new scope or explicitly dropping it.
    match rtc.poll_output()? {
        Output::Timeout(deadline) => assert_eq!(deadline, now),
        output => panic!("keyframe request must schedule an immediate timeout: {output:?}"),
    }
    Ok(())
}
