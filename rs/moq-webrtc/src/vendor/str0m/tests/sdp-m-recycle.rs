use std::time::Instant;

mod common;
use common::TestRtc;
use common::init_crypto_default;
use common::init_log;
use common::negotiate;
use str0m::Rtc;
use str0m::change::SdpOffer;
use str0m::format::Codec;
use str0m::format::CodecSpec;
use str0m::format::FormatParams;
use str0m::format::PayloadParams;
use str0m::media::Direction;
use str0m::media::Frequency;
use str0m::media::MediaKind;
use str0m::media::Mid;
use str0m::media::Pt;
use tracing::Span;
use tracing::info_span;

#[test]
pub fn stop_media_then_remote_recycles_slot() {
    init_log();
    init_crypto_default();

    // L stops its video m-line, then receives an offer that recycles the
    // resulting port=0 slot with a new mid (RFC 8829 §5.2.2).
    let (mut l, mut r) = with_params(info_span!("L"), &[vp8(100)], info_span!("R"), &[vp8(100)]);

    let old_mid = l._mids()[0];

    negotiate(&mut l, &mut r, |change| {
        change.stop_media(old_mid);
    });

    assert!(l.media(old_mid).unwrap().stopped());
    assert!(r.media(old_mid).unwrap().stopped());

    // str0m's own SdpApi doesn't produce recycling offers, so take an
    // offer from R (which appends a new m-line) and rewrite it so the
    // new m-line lives at the index of the stopped one - what a browser
    // would emit per RFC 8829 §5.2.2.
    let offer_from_r = r
        .span
        .in_scope(|| {
            let mut change = r.rtc.sdp_api();
            change.add_media(MediaKind::Video, Direction::SendOnly, None, None, None);
            change.apply()
        })
        .expect("R has a change to apply");

    let offer_sdp = offer_from_r.0.to_sdp_string();
    let recycled_offer_sdp = rewrite_offer_to_recycle_stopped_slot(&offer_sdp, old_mid);

    let recycled_offer =
        SdpOffer::from_sdp_string(&recycled_offer_sdp).expect("recycled offer parses");

    let answer = l.span.in_scope(|| {
        l.rtc
            .sdp_api()
            .accept_offer(recycled_offer)
            .expect("accept")
    });

    let answer_sdp = answer.to_sdp_string();

    // RFC 3264 §6: the answer must have the same m-line count as the
    // offer. Without recycling, str0m appends a second m-line and the
    // answer is invalid.
    let count_m_lines = |s: &str| s.lines().filter(|l| l.starts_with("m=")).count();
    let offer_m_lines = count_m_lines(&recycled_offer_sdp);
    let answer_m_lines = count_m_lines(&answer_sdp);
    assert_eq!(
        answer_m_lines, offer_m_lines,
        "answer m-line count ({answer_m_lines}) differs from offer m-line count ({offer_m_lines})\n\n\
         rewritten offer:\n{recycled_offer_sdp}\n\nanswer:\n{answer_sdp}",
    );
}

#[test]
pub fn recycling_rejected_for_non_stopped_slot() {
    init_log();
    init_crypto_default();

    // L holds an active video m-line. An offer arrives that tries to
    // recycle that active slot with a new mid (no prior stop). str0m must
    // refuse rather than install a second Media at the same index.
    let (mut l, mut r) = with_params(info_span!("L"), &[vp8(100)], info_span!("R"), &[vp8(100)]);

    let active_mid = l._mids()[0];

    // Sanity: the mid is active, not stopped.
    assert!(!l.media(active_mid).unwrap().stopped());
    assert!(!l.media(active_mid).unwrap().disabled());

    // Take an ordinary offer from R adding a new m-line, then rewrite it
    // so the new m-line squats the slot of L's active mid.
    let offer_from_r = r
        .span
        .in_scope(|| {
            let mut change = r.rtc.sdp_api();
            change.add_media(MediaKind::Video, Direction::SendOnly, None, None, None);
            change.apply()
        })
        .expect("R has a change to apply");

    let offer_sdp = offer_from_r.0.to_sdp_string();
    let bad_offer_sdp = rewrite_offer_to_recycle_stopped_slot(&offer_sdp, active_mid);
    let bad_offer = SdpOffer::from_sdp_string(&bad_offer_sdp).expect("offer parses");

    let result = l.span.in_scope(|| l.rtc.sdp_api().accept_offer(bad_offer));

    assert!(
        result.is_err(),
        "accept_offer must reject a recycling attempt against a non-stopped slot, got Ok:\n{}",
        bad_offer_sdp
    );
}

#[test]
pub fn stop_media_recycled_slot_state() {
    init_log();
    init_crypto_default();

    // After a recycled offer is accepted, verify the post-recycle state
    // of the surviving Media objects. The happy-path test only asserts
    // answer m-line count; this pins down the full state.
    let (mut l, mut r) = with_params(info_span!("L"), &[vp8(100)], info_span!("R"), &[vp8(100)]);

    let old_mid = l._mids()[0];

    negotiate(&mut l, &mut r, |change| {
        change.stop_media(old_mid);
    });

    let offer_from_r = r
        .span
        .in_scope(|| {
            let mut change = r.rtc.sdp_api();
            change.add_media(MediaKind::Video, Direction::SendOnly, None, None, None);
            change.apply()
        })
        .expect("R has a change to apply");

    let recycled_offer_sdp =
        rewrite_offer_to_recycle_stopped_slot(&offer_from_r.0.to_sdp_string(), old_mid);
    let recycled_offer = SdpOffer::from_sdp_string(&recycled_offer_sdp).expect("parses");

    // The new mid is whichever mid in the recycled SDP isn't old_mid.
    let new_mid = new_mid_in_sdp(&recycled_offer_sdp, old_mid);

    l.span.in_scope(|| {
        l.rtc
            .sdp_api()
            .accept_offer(recycled_offer)
            .expect("accept");
    });

    // L side: old mid is retired, new mid is live and not stopped.
    assert!(
        !l._mids().contains(&old_mid),
        "retired mid {old_mid} must not appear in L's medias after recycle, got {:?}",
        l._mids()
    );
    assert!(
        l._mids().contains(&new_mid),
        "recycled mid {new_mid} must appear in L's medias, got {:?}",
        l._mids()
    );
    assert!(l.media(old_mid).is_none(), "old Media must be gone on L");

    let new_media_l = l.media(new_mid).expect("new Media on L");
    assert!(!new_media_l.stopped(), "recycled Media must not be stopped");
    assert!(
        !new_media_l.disabled(),
        "recycled Media must not be disabled"
    );
    assert_eq!(
        new_media_l.kind(),
        MediaKind::Video,
        "recycled Media kind must match the offer"
    );
    // R offered sendonly, so L's inverted direction is recvonly.
    assert_eq!(new_media_l.direction(), Direction::RecvOnly);
    assert_eq!(
        new_media_l.remote_pts(),
        &[Pt::new_with_value(100)],
        "recycled Media must have remote_pts populated"
    );
}

#[test]
pub fn stop_media_and_recycle_then_negotiate_again() {
    init_log();
    init_crypto_default();

    // After accepting a recycled offer, L must still be able to generate
    // a valid subsequent offer of its own. This exercises that session
    // state (medias, streams, index bookkeeping) is consistent for the
    // side that processed the recycling.
    //
    // We don't round-trip R here: R sent a non-recycled offer that was
    // rewritten out-of-band, so from R's own perspective the resulting
    // answer is malformed. That's a synthetic-test artifact, not a
    // real-world scenario (browsers always answer in-shape).
    let (mut l, mut r) = with_params(info_span!("L"), &[vp8(100)], info_span!("R"), &[vp8(100)]);

    let old_mid = l._mids()[0];

    negotiate(&mut l, &mut r, |change| {
        change.stop_media(old_mid);
    });

    let offer_from_r = r
        .span
        .in_scope(|| {
            let mut change = r.rtc.sdp_api();
            change.add_media(MediaKind::Video, Direction::SendOnly, None, None, None);
            change.apply()
        })
        .expect("R has a change to apply");

    let recycled_offer_sdp =
        rewrite_offer_to_recycle_stopped_slot(&offer_from_r.0.to_sdp_string(), old_mid);
    let recycled_offer = SdpOffer::from_sdp_string(&recycled_offer_sdp).expect("parses");

    l.span.in_scope(|| {
        l.rtc
            .sdp_api()
            .accept_offer(recycled_offer)
            .expect("accept")
    });

    // L must be able to create another offer cleanly. This would fail if
    // `as_media_lines` disagrees with the post-recycle `medias` list.
    let follow_up = l
        .span
        .in_scope(|| {
            let mut change = l.rtc.sdp_api();
            change.add_media(MediaKind::Video, Direction::SendOnly, None, None, None);
            change.apply()
        })
        .expect("L can produce a follow-up offer");

    let follow_up_sdp = follow_up.0.to_sdp_string();

    // Retired mid must not reappear in L's offer.
    let old_mid_str = old_mid.to_string();
    assert!(
        !follow_up_sdp
            .lines()
            .any(|l| l.strip_prefix("a=mid:").map(str::trim) == Some(old_mid_str.as_str())),
        "retired mid resurfaced in L's follow-up offer:\n{}",
        follow_up_sdp
    );
    // The follow-up SDP must have exactly 2 m-lines (recycled + new).
    let m_line_count = follow_up_sdp
        .lines()
        .filter(|l| l.starts_with("m="))
        .count();
    assert_eq!(
        m_line_count, 2,
        "expected 2 m-lines in follow-up offer, got:\n{}",
        follow_up_sdp
    );
    assert!(!l._mids().contains(&old_mid));
}

#[test]
pub fn recycling_with_media_type_change() {
    init_log();
    init_crypto_default();

    // JSEP §5.2.2 allows a recycled slot to change media type. Stop an
    // audio m-line and have the remote reoffer with a video m-line at
    // the same slot. Both peers need audio *and* video codecs, so we
    // construct them manually rather than via with_params.
    let build = |span: Span| {
        let mut b = Rtc::builder().clear_codecs();
        b.codec_config().add_config(
            Pt::new_with_value(111),
            None,
            Codec::Opus,
            Frequency::FORTY_EIGHT_KHZ,
            Some(2),
            FormatParams::default(),
        );
        b.codec_config().add_config(
            Pt::new_with_value(100),
            None,
            Codec::Vp8,
            Frequency::NINETY_KHZ,
            None,
            FormatParams::default(),
        );
        TestRtc::new_with_rtc(span, b.build(Instant::now()))
    };
    let mut l = build(info_span!("L"));
    let mut r = build(info_span!("R"));

    negotiate(&mut l, &mut r, |change| {
        change.add_media(MediaKind::Audio, Direction::SendRecv, None, None, None);
    });

    let old_mid = l._mids()[0];
    assert_eq!(l.media(old_mid).unwrap().kind(), MediaKind::Audio);

    negotiate(&mut l, &mut r, |change| {
        change.stop_media(old_mid);
    });

    let offer_from_r = r
        .span
        .in_scope(|| {
            let mut change = r.rtc.sdp_api();
            change.add_media(MediaKind::Video, Direction::SendOnly, None, None, None);
            change.apply()
        })
        .expect("R has a change to apply");

    let recycled_sdp =
        rewrite_offer_to_recycle_stopped_slot(&offer_from_r.0.to_sdp_string(), old_mid);
    let recycled_offer = SdpOffer::from_sdp_string(&recycled_sdp).expect("parses");
    let new_mid = new_mid_in_sdp(&recycled_sdp, old_mid);

    let result = l
        .span
        .in_scope(|| l.rtc.sdp_api().accept_offer(recycled_offer));
    assert!(
        result.is_ok(),
        "accepting a recycle that changes media type (audio->video) should succeed per JSEP §5.2.2: {:?}",
        result.err()
    );

    let recycled = l.media(new_mid).expect("new Media present");
    assert_eq!(
        recycled.kind(),
        MediaKind::Video,
        "recycled Media kind must reflect the new m-line type"
    );
    assert!(!recycled.stopped());
}

#[test]
pub fn recycling_in_answer_produces_error() {
    init_log();
    init_crypto_default();

    // JSEP §5.2.2 only permits recycling in offers. An answer that
    // introduces a new mid at the index of a stopped slot is malformed.
    // str0m's `ensure_correct_answer` should reject it rather than
    // silently retiring our Media.
    let (mut l, mut r) = with_params(info_span!("L"), &[vp8(100)], info_span!("R"), &[vp8(100)]);

    let old_mid = l._mids()[0];

    negotiate(&mut l, &mut r, |change| {
        change.stop_media(old_mid);
    });

    // L initiates a fresh offer adding a new video m-line; R will answer.
    let (offer_from_l, pending_l) = l
        .span
        .in_scope(|| {
            let mut change = l.rtc.sdp_api();
            change.add_media(MediaKind::Video, Direction::SendOnly, None, None, None);
            change.apply()
        })
        .expect("L has a change to apply");

    let answer_from_r = r
        .span
        .in_scope(|| r.rtc.sdp_api().accept_offer(offer_from_l).expect("accept"));

    // Rewrite R's answer so the new mid's m-line squats the old stopped
    // slot. This is the malformed shape we want L to reject.
    let malformed_answer_sdp =
        rewrite_offer_to_recycle_stopped_slot(&answer_from_r.to_sdp_string(), old_mid);
    let malformed_answer =
        str0m::change::SdpAnswer::from_sdp_string(&malformed_answer_sdp).expect("parses");

    let result = l
        .span
        .in_scope(|| l.rtc.sdp_api().accept_answer(pending_l, malformed_answer));

    assert!(
        result.is_err(),
        "accept_answer must reject a recycled answer, got Ok:\n{}",
        malformed_answer_sdp
    );
}

// -------------------------------------------------------------------------
// Helpers
// -------------------------------------------------------------------------

/// Moves the trailing (newly added) m-line into the slot occupied by the
/// m-line for `old_mid`, and updates the BUNDLE group accordingly. When
/// `old_mid` belongs to a stopped m-line (port=0) this produces the offer
/// shape a browser emits when recycling the slot per RFC 8829 §5.2.2.
fn rewrite_offer_to_recycle_stopped_slot(sdp: &str, old_mid: Mid) -> String {
    // Split into sections, each starting with "m=". The first section is
    // the session-level block before any m-line.
    let mut sections: Vec<String> = Vec::new();
    let mut current = String::new();
    for line in sdp.split_inclusive("\r\n") {
        if line.starts_with("m=") && !current.is_empty() {
            sections.push(std::mem::take(&mut current));
        }
        current.push_str(line);
    }
    if !current.is_empty() {
        sections.push(current);
    }

    let session_block = sections.remove(0);

    // Find the stopped section and the trailing new section.
    let old_mid_attr = format!("a=mid:{old_mid}\r\n");
    let stopped_idx = sections
        .iter()
        .position(|s| s.contains(&old_mid_attr))
        .expect("stopped section present");
    let new_section = sections.pop().expect("new section present");

    sections[stopped_idx] = new_section;

    // Parse the new mid so we can rewrite the BUNDLE group.
    let new_mid = sections[stopped_idx]
        .lines()
        .find_map(|l| l.strip_prefix("a=mid:"))
        .expect("new section has a=mid:")
        .trim()
        .to_string();

    let old_mid_str = old_mid.to_string();
    let session_block = session_block
        .lines()
        .map(|l| {
            if let Some(rest) = l.strip_prefix("a=group:BUNDLE ") {
                let mut mids: Vec<&str> = rest
                    .split_whitespace()
                    .filter(|m| *m != old_mid_str)
                    .collect();
                if !mids.contains(&new_mid.as_str()) {
                    mids.push(new_mid.as_str());
                }
                format!("a=group:BUNDLE {}", mids.join(" "))
            } else {
                l.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("\r\n")
        + "\r\n";

    let mut out = session_block;
    for s in sections {
        out.push_str(&s);
    }
    out
}

/// Find the mid in `sdp` that isn't `old_mid`. Panics if zero or more than one match.
fn new_mid_in_sdp(sdp: &str, old_mid: Mid) -> Mid {
    let old_str = old_mid.to_string();
    let mut found: Option<Mid> = None;
    for line in sdp.lines() {
        if let Some(rest) = line.strip_prefix("a=mid:") {
            let m = rest.trim();
            if m == old_str {
                continue;
            }
            assert!(
                found.is_none(),
                "more than one non-old mid in sdp:\n{}",
                sdp
            );
            found = Some(m.into());
        }
    }
    found.expect("a new mid must be present in the recycled sdp")
}

fn with_params(
    span_l: Span,
    params_l: &[PayloadParams],
    span_r: Span,
    params_r: &[PayloadParams],
) -> (TestRtc, TestRtc) {
    let mut l = build_params(span_l, params_l);
    let mut r = build_params(span_r, params_r);

    let kind = params_l
        .first()
        .map(|p| p.spec().codec.kind())
        .unwrap_or(MediaKind::Audio);

    negotiate(&mut l, &mut r, |change| {
        change.add_media(kind, Direction::SendRecv, None, None, None);
    });

    (l, r)
}

fn build_params(span: Span, params: &[PayloadParams]) -> TestRtc {
    let mut b = Rtc::builder().clear_codecs();
    let config = b.codec_config();
    for p in params {
        config.add_config(
            p.pt(),
            p.resend(),
            p.spec().codec,
            p.spec().clock_rate,
            p.spec().channels,
            p.spec().format,
        );
    }
    TestRtc::new_with_rtc(span, b.build(Instant::now()))
}

fn vp8(pt: u8) -> PayloadParams {
    PayloadParams::new(
        pt.into(),
        None,
        CodecSpec {
            codec: Codec::Vp8,
            channels: None,
            clock_rate: Frequency::NINETY_KHZ,
            format: FormatParams::default(),
        },
    )
}
