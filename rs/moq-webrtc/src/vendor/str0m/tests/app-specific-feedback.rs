use std::sync::Arc;
use std::time::Duration;

use str0m::media::MediaKind;
use str0m::rtp::Ssrc;
use str0m::{Event, RtcError};

mod common;
use common::{connect_l_r, init_crypto_default, init_log, progress};

/// Test that AppSpecificFeedback sent from one peer via DirectApi
/// is received by the other peer as an Event::AppSpecificFeedback.
#[test]
pub fn app_specific_feedback_direct_api() -> Result<(), RtcError> {
    init_log();
    init_crypto_default();

    let (mut l, mut r) = connect_l_r();

    // Declare a video media line on both sides so we have valid SSRCs
    // for routing the feedback.
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

    // Build a test payload — a simple "TestAppFeedback" message.
    let test_payload: Vec<u8> = vec![
        0x54, 0x46, // magic: "TF" (TestFeedback)
        0x00, 0x01, // version: 1
        0xDE, 0xAD, 0xBE, 0xEF, // application data
        0xCA, 0xFE, 0xBA, 0xBE, // more application data
    ];

    // L sends AppSpecificFeedback to R
    l.direct_api().send_app_specific_feedback(
        ssrc_l, // sender_ssrc
        ssrc_r, // media_ssrc
        test_payload.clone(),
    );

    // Progress until R receives the feedback event
    let deadline = l.last + Duration::from_secs(5);
    let mut received = false;

    loop {
        if l.last > deadline || r.last > deadline {
            break;
        }
        progress(&mut l, &mut r)?;

        // Check R's events for the feedback
        for (_time, event) in r.events.drain(..) {
            if let Event::AppSpecificFeedback(fb) = event {
                assert_eq!(fb.sender_ssrc, ssrc_l);
                assert_eq!(fb.media_ssrc, ssrc_r);
                assert_eq!(&fb.payload[..test_payload.len()], &test_payload[..]);
                received = true;
            }
        }

        if received {
            break;
        }
    }

    assert!(
        received,
        "R should have received AppSpecificFeedback from L"
    );

    Ok(())
}

/// Test bidirectional AppSpecificFeedback — both peers send and receive.
#[test]
pub fn app_specific_feedback_bidirectional() -> Result<(), RtcError> {
    init_log();
    init_crypto_default();

    let (mut l, mut r) = connect_l_r();

    let mid = "vid".into();
    let ssrc_l: Ssrc = 100.into();
    let ssrc_r: Ssrc = 200.into();

    l.direct_api().declare_media(mid, MediaKind::Video);
    l.direct_api().declare_stream_tx(ssrc_l, None, mid, None);
    l.direct_api().expect_stream_rx(ssrc_r, None, mid, None);

    r.direct_api().declare_media(mid, MediaKind::Video);
    r.direct_api().declare_stream_tx(ssrc_r, None, mid, None);
    r.direct_api().expect_stream_rx(ssrc_l, None, mid, None);

    let max = l.last.max(r.last);
    l.last = max;
    r.last = max;

    let payload_l_to_r: Vec<u8> = vec![0x01, 0x02, 0x03, 0x04];
    let payload_r_to_l: Vec<u8> = vec![0xAA, 0xBB, 0xCC, 0xDD];

    // Both sides send feedback
    l.direct_api()
        .send_app_specific_feedback(ssrc_l, ssrc_r, payload_l_to_r.clone());
    r.direct_api()
        .send_app_specific_feedback(ssrc_r, ssrc_l, payload_r_to_l.clone());

    let deadline = l.last + Duration::from_secs(5);
    let mut r_received = false;
    let mut l_received = false;

    loop {
        if (r_received && l_received) || l.last > deadline || r.last > deadline {
            break;
        }
        progress(&mut l, &mut r)?;

        for (_time, event) in r.events.drain(..) {
            if let Event::AppSpecificFeedback(fb) = event {
                assert_eq!(&fb.payload[..payload_l_to_r.len()], &payload_l_to_r[..]);
                r_received = true;
            }
        }

        for (_time, event) in l.events.drain(..) {
            if let Event::AppSpecificFeedback(fb) = event {
                assert_eq!(&fb.payload[..payload_r_to_l.len()], &payload_r_to_l[..]);
                l_received = true;
            }
        }
    }

    assert!(r_received, "R should have received feedback from L");
    assert!(l_received, "L should have received feedback from R");

    Ok(())
}

/// Test that multiple AppSpecificFeedback messages are delivered in order.
#[test]
pub fn app_specific_feedback_multiple_messages() -> Result<(), RtcError> {
    init_log();
    init_crypto_default();

    let (mut l, mut r) = connect_l_r();

    let mid = "vid".into();
    let ssrc_l: Ssrc = 100.into();
    let ssrc_r: Ssrc = 200.into();

    l.direct_api().declare_media(mid, MediaKind::Video);
    l.direct_api().declare_stream_tx(ssrc_l, None, mid, None);
    l.direct_api().expect_stream_rx(ssrc_r, None, mid, None);

    r.direct_api().declare_media(mid, MediaKind::Video);
    r.direct_api().declare_stream_tx(ssrc_r, None, mid, None);
    r.direct_api().expect_stream_rx(ssrc_l, None, mid, None);

    let max = l.last.max(r.last);
    l.last = max;
    r.last = max;

    // Send 3 messages sequentially — at most one pending at a time.
    let mut received_payloads: Vec<Arc<[u8]>> = Vec::new();

    for i in 0u8..3 {
        let payload = vec![i, i + 10, i + 20, i + 30];
        l.direct_api()
            .send_app_specific_feedback(ssrc_l, ssrc_r, payload);

        let deadline = l.last + Duration::from_secs(5);
        loop {
            if l.last > deadline || r.last > deadline {
                break;
            }
            progress(&mut l, &mut r)?;

            let mut found = false;
            for (_time, event) in r.events.drain(..) {
                if let Event::AppSpecificFeedback(fb) = event {
                    received_payloads.push(fb.payload);
                    found = true;
                }
            }
            if found {
                break;
            }
        }
    }

    assert_eq!(received_payloads.len(), 3, "Should receive all 3 messages");
    for (i, payload) in received_payloads.iter().enumerate() {
        let i = i as u8;
        assert_eq!(payload[0], i);
        assert_eq!(payload[1], i + 10);
        assert_eq!(payload[2], i + 20);
        assert_eq!(payload[3], i + 30);
    }

    Ok(())
}
