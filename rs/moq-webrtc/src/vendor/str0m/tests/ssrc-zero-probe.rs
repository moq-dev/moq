use std::time::{Duration, Instant};

use str0m::bwe::{Bitrate, BweKind};
use str0m::media::{Direction, MediaKind};
use str0m::rtp::{ExtensionMap, RawPacket, RtpWrite};
use str0m::{Event, Rtc, RtcError};

mod common;
use common::{TestRtc, connect_l_r_with_rtc, init_crypto_default, progress};

#[test]
fn direct_audio_only_before_media() -> Result<(), RtcError> {
    let (mut l, mut r) = peers(true);
    l.direct_api().declare_media("aud".into(), MediaKind::Audio);
    r.direct_api().declare_media("aud".into(), MediaKind::Audio);
    run(&mut l, &mut r, Duration::from_secs(2))?;
    assert_feedback_and_no_media(&l, &r);
    Ok(())
}

#[test]
fn sdp_before_media_and_after_video_removal() -> Result<(), RtcError> {
    let (mut l, mut r) = peers(false);
    let mut change = l.sdp_api();
    let mid = change.add_media(MediaKind::Video, Direction::SendOnly, None, None, None);
    let (offer, pending) = change.apply().unwrap();
    let answer = r.sdp_api().accept_offer(offer)?;
    l.sdp_api().accept_answer(pending, answer)?;
    // SDP creates a send stream eagerly. Probing must also work without it.
    let initial_ssrc = l.direct_api().stream_tx_by_mid(mid, None).unwrap().ssrc();
    l.direct_api().remove_stream_tx(initial_ssrc);
    run(&mut l, &mut r, Duration::from_secs(2))?;
    assert_feedback_and_no_media(&l, &r);

    // Start a fresh stream only after measuring capacity, with no RTX/padding setup.
    let pt = l.params_vp8().pt();
    let ssrc = 4242.into();
    let now = l.last;
    l.direct_api()
        .declare_stream_tx(ssrc, None, mid, None)
        .write_rtp(RtpWrite::new(pt, 1.into(), 90000, now, vec![1; 1000]));
    run(&mut l, &mut r, Duration::from_millis(200))?;
    assert!(r.events.iter().any(|(_, event)| matches!(event,
        Event::RtpPacket(p) if p.header.ssrc == ssrc && p.payload.len() == 1000
    )));
    assert!(l.direct_api().remove_stream_tx(ssrc));
    l.events.clear();
    r.events.clear();
    run(&mut l, &mut r, Duration::from_secs(12))?;
    assert!(probes(&l) >= 5, "probing must survive video stream removal");
    assert!(
        !r.events
            .iter()
            .any(|(_, event)| matches!(event, Event::RtpPacket(_) | Event::MediaData(_)))
    );
    Ok(())
}

#[test]
fn missing_feedback_keeps_probes_bounded() -> Result<(), RtcError> {
    let (mut l, mut r) = peers(false);
    l.direct_api().declare_media("aud".into(), MediaKind::Audio);
    r.direct_api().declare_media("aud".into(), MediaKind::Audio);
    run(&mut l, &mut r, Duration::from_secs(30))?;
    let count = probes(&l);
    assert!(count >= 5, "initial probes must not require feedback first");
    assert!(
        count < 400,
        "missing feedback caused unbounded padding: {count}"
    );
    assert!(
        !l.events.iter().any(|(_, event)| matches!(event,
            Event::Probe(probe) if probe.is_alr_probe()
        )),
        "periodic ALR probes must wait for transport feedback"
    );
    r.direct_api().enable_twcc_feedback();
    run(&mut l, &mut r, Duration::from_secs(12))?;
    assert_feedback_and_no_media(&l, &r);
    assert!(
        l.events.iter().any(|(_, event)| matches!(event,
            Event::Probe(probe) if probe.is_alr_probe()
        )),
        "ALR probing should start once feedback arrives"
    );
    Ok(())
}

#[test]
fn no_twcc_extension_no_probes() -> Result<(), RtcError> {
    common::init_log();
    init_crypto_default();
    let now = Instant::now();
    let rtc = Rtc::builder()
        .enable_bwe(Some(Bitrate::kbps(300)))
        .set_extension_map(ExtensionMap::empty())
        .enable_raw_packets(true)
        .build(now);
    let (mut l, mut r) = connect_l_r_with_rtc(rtc, Rtc::builder().build(now));
    l.direct_api().declare_media("aud".into(), MediaKind::Audio);
    r.direct_api().declare_media("aud".into(), MediaKind::Audio);
    run(&mut l, &mut r, Duration::from_secs(2))?;
    assert_eq!(probes(&l), 0);
    assert_no_twcc_estimates(&l);
    Ok(())
}

#[test]
fn idle_capacity_collapse_and_recovery() -> Result<(), RtcError> {
    let (mut l, mut r) = peers(true);
    l.direct_api().declare_media("aud".into(), MediaKind::Audio);
    r.direct_api().declare_media("aud".into(), MediaKind::Audio);
    r.set_netem(
        netem::NetemConfig::new()
            .link(Bitrate::mbps(2), netem::DataSize::kbytes(10))
            .seed(42),
    );
    run(&mut l, &mut r, Duration::from_secs(3))?;
    assert_feedback_and_no_media(&l, &r);
    l.events.clear();
    r.set_netem(
        netem::NetemConfig::new()
            .link(Bitrate::kbps(10), netem::DataSize::kbytes(1))
            .seed(42),
    );
    run(&mut l, &mut r, Duration::from_secs(15))?;
    assert!(
        probes(&l) < 2000,
        "capacity collapse must not cause continuous probing"
    );
    l.events.clear();
    r.set_netem(
        netem::NetemConfig::new()
            .link(Bitrate::mbps(2), netem::DataSize::kbytes(10))
            .seed(42),
    );
    run(&mut l, &mut r, Duration::from_secs(20))?;
    assert_feedback_and_no_media(&l, &r);
    Ok(())
}

#[test]
fn sdp_without_transport_sequence_extension_does_not_probe() -> Result<(), RtcError> {
    let (mut l, mut r) = peers(false);
    let mut change = l.sdp_api();
    change.add_media(MediaKind::Audio, Direction::SendOnly, None, None, None);
    let (offer, pending) = change.apply().unwrap();
    let answer = r.sdp_api().accept_offer(offer)?;
    let sdp = answer
        .to_sdp_string()
        .lines()
        .filter(|s| !s.contains("transport-wide-cc"))
        .collect::<Vec<_>>()
        .join("\r\n")
        + "\r\n";
    l.sdp_api().accept_answer(
        pending,
        str0m::change::SdpAnswer::from_sdp_string(&sdp).unwrap(),
    )?;
    run(&mut l, &mut r, Duration::from_secs(2))?;
    assert_eq!(probes(&l), 0);
    assert_no_twcc_estimates(&l);
    Ok(())
}

#[test]
fn sdp_without_transport_feedback_does_not_probe() -> Result<(), RtcError> {
    let (mut l, mut r) = peers(false);
    let mut change = l.sdp_api();
    change.add_media(MediaKind::Audio, Direction::SendOnly, None, None, None);
    let (offer, pending) = change.apply().unwrap();
    let answer = r.sdp_api().accept_offer(offer)?;
    let sdp = answer
        .to_sdp_string()
        .lines()
        .filter(|s| !s.contains("transport-cc"))
        .collect::<Vec<_>>()
        .join("\r\n")
        + "\r\n";
    l.sdp_api().accept_answer(
        pending,
        str0m::change::SdpAnswer::from_sdp_string(&sdp).unwrap(),
    )?;
    run(&mut l, &mut r, Duration::from_secs(2))?;
    assert_eq!(probes(&l), 0);
    assert_no_twcc_estimates(&l);
    Ok(())
}

#[test]
fn probes_continue_while_sending_only_audio() -> Result<(), RtcError> {
    let (mut l, mut r) = peers(true);
    let mid = "aud".into();
    l.direct_api().declare_media(mid, MediaKind::Audio);
    r.direct_api().declare_media(mid, MediaKind::Audio);
    let ssrc = 4242.into();
    l.direct_api().declare_stream_tx(ssrc, None, mid, None);
    let pt = l.params_opus().pt();
    for seq in 0..400u64 {
        let now = l.last;
        l.direct_api()
            .stream_tx(&ssrc)
            .unwrap()
            .write_rtp(RtpWrite::new(
                pt,
                seq.into(),
                seq as u32 * 960,
                now,
                vec![1; 80],
            ));
        run(&mut l, &mut r, Duration::from_millis(20))?;
    }
    assert!(probes(&l) >= 5);
    assert!(r.events.iter().any(|(_, event)| matches!(event,
        Event::RtpPacket(p) if p.header.ssrc == ssrc && p.payload.len() == 80
    )));
    assert!(!r.events.iter().any(|(_, event)| matches!(event,
        Event::RtpPacket(p) if *p.header.ssrc == 0
    )));
    Ok(())
}

#[test]
fn renegotiating_inactive_signals_probe_state_transitions() -> Result<(), RtcError> {
    let (mut l, mut r) = peers(true);
    let mut change = l.sdp_api();
    let mid = change.add_media(MediaKind::Audio, Direction::SendOnly, None, None, None);
    let (offer, pending) = change.apply().unwrap();
    let answer = r.sdp_api().accept_offer(offer)?;
    l.sdp_api().accept_answer(pending, answer)?;
    run(&mut l, &mut r, Duration::from_secs(2))?;
    assert!(probes(&l) > 0);
    let last_estimate = l
        .events
        .iter()
        .rev()
        .find_map(|(_, event)| match event {
            Event::EgressBitrateEstimate(BweKind::Twcc {
                estimate,
                can_probe: true,
            }) => Some(*estimate),
            _ => None,
        })
        .unwrap();
    assert!(last_estimate > Bitrate::kbps(300));

    let mut change = l.sdp_api();
    change.set_direction(mid, Direction::Inactive);
    let (offer, pending) = change.apply().unwrap();
    let answer = r.sdp_api().accept_offer(offer)?;
    l.sdp_api().accept_answer(pending, answer)?;
    l.events.clear();
    run(&mut l, &mut r, Duration::from_secs(2))?;
    assert_eq!(probes(&l), 0);
    let estimates: Vec<_> = l
        .events
        .iter()
        .filter_map(|(_, event)| match event {
            Event::EgressBitrateEstimate(kind) => Some(kind),
            _ => None,
        })
        .collect();
    assert_eq!(
        estimates,
        vec![&BweKind::Twcc {
            estimate: last_estimate,
            can_probe: false,
        }]
    );
    l.events.clear();

    let mut change = l.sdp_api();
    change.set_direction(mid, Direction::SendOnly);
    let (offer, pending) = change.apply().unwrap();
    let answer = r.sdp_api().accept_offer(offer)?;
    l.sdp_api().accept_answer(pending, answer)?;
    run(&mut l, &mut r, Duration::from_millis(10))?;
    assert!(l.events.iter().any(|(_, event)| matches!(
        event,
        Event::EgressBitrateEstimate(BweKind::Twcc {
            can_probe: true,
            ..
        })
    )));
    Ok(())
}

#[test]
fn video_padding_before_srtp_does_not_spin() -> Result<(), RtcError> {
    init_crypto_default();
    let now = Instant::now();
    let mut l = Rtc::builder()
        .enable_bwe(Some(Bitrate::kbps(300)))
        .build(now);
    let mut r = Rtc::builder().build(now);
    let mut change = l.sdp_api();
    change.add_media(MediaKind::Video, Direction::SendOnly, None, None, None);
    let (offer, pending) = change.apply().unwrap();
    let answer = r.sdp_api().accept_offer(offer)?;
    l.sdp_api().accept_answer(pending, answer)?;
    l.bwe().set_bitrate(Bitrate::mbps(1), Bitrate::mbps(2));

    // Drain immediate work without advancing the supplied clock. There are no
    // SRTP keys yet, so unsendable padding must not keep the deadline at `now`.
    for _ in 0..100 {
        l.handle_input(str0m::Input::Timeout(now))?;
        match l.poll_output()? {
            str0m::Output::Timeout(deadline) if deadline > now => return Ok(()),
            str0m::Output::Event(Event::EgressBitrateEstimate(BweKind::Twcc { .. })) => {
                panic!("TWCC estimate before SRTP readiness")
            }
            str0m::Output::Transmit(_) => panic!("no candidates or keys have been installed"),
            _ => {}
        }
    }
    panic!("padding before SRTP readiness kept requesting immediate timeouts");
}

#[test]
fn video_without_twcc_does_not_hide_audio_probe_source() -> Result<(), RtcError> {
    for remove_extension in [false, true] {
        let (mut l, mut r) = peers(false);
        let mut change = l.sdp_api();
        change.add_media(MediaKind::Audio, Direction::SendOnly, None, None, None);
        change.add_media(MediaKind::Video, Direction::SendOnly, None, None, None);
        let (offer, pending) = change.apply().unwrap();
        let answer = r.sdp_api().accept_offer(offer)?;
        let mut video = false;
        let sdp = answer
            .to_sdp_string()
            .lines()
            .filter(|line| {
                if line.starts_with("m=") {
                    video = line.starts_with("m=video");
                }
                // Keep RTX negotiated for video, but negotiate TWCC feedback only on audio.
                !(video
                    && (line.contains("transport-cc")
                        || (remove_extension && line.contains("transport-wide-cc"))))
            })
            .collect::<Vec<_>>()
            .join("\r\n")
            + "\r\n";
        l.sdp_api().accept_answer(
            pending,
            str0m::change::SdpAnswer::from_sdp_string(&sdp).unwrap(),
        )?;
        run(&mut l, &mut r, Duration::from_secs(2))?;
        assert!(
            l.events.iter().any(|(_, event)| matches!(event,
                Event::EgressBitrateEstimate(BweKind::Twcc { estimate: rate, can_probe: true })
            if *rate > Bitrate::kbps(300)
            )),
            "video RTX without TWCC must not prevent capacity discovery on audio"
        );
        assert_feedback_and_no_media(&l, &r);
    }
    Ok(())
}

#[test]
fn negotiated_video_rtx_remains_preferred_over_audio_fallback() -> Result<(), RtcError> {
    let (mut l, mut r) = peers(false);
    let mut change = l.sdp_api();
    change.add_media(MediaKind::Audio, Direction::SendOnly, None, None, None);
    change.add_media(MediaKind::Video, Direction::SendOnly, None, None, None);
    let (offer, pending) = change.apply().unwrap();
    let answer = r.sdp_api().accept_offer(offer)?;
    l.sdp_api().accept_answer(pending, answer)?;
    run(&mut l, &mut r, Duration::from_secs(2))?;
    assert_eq!(
        probes(&l),
        0,
        "negotiated video RTX should supply probe padding"
    );
    assert!(l.events.iter().any(|(_, event)| matches!(event,
        Event::EgressBitrateEstimate(BweKind::Twcc { estimate: rate, can_probe: true })
            if *rate > Bitrate::kbps(300)
    )));
    Ok(())
}

#[test]
fn readiness_emits_initial_estimate_without_feedback() -> Result<(), RtcError> {
    let (mut l, mut r) = peers(false);
    run(&mut l, &mut r, Duration::from_secs(1))?;
    assert_no_twcc_estimates(&l);
    l.direct_api().declare_media("aud".into(), MediaKind::Audio);
    r.direct_api().declare_media("aud".into(), MediaKind::Audio);
    run(&mut l, &mut r, Duration::from_millis(10))?;
    let first = l.events.iter().find_map(|(_, event)| match event {
        Event::EgressBitrateEstimate(BweKind::Twcc {
            estimate: rate,
            can_probe: true,
        }) => Some(*rate),
        _ => None,
    });
    assert_eq!(first, Some(Bitrate::kbps(300)));
    Ok(())
}

fn assert_no_twcc_estimates(rtc: &TestRtc) {
    assert!(
        !rtc.events
            .iter()
            .any(|(_, event)| matches!(event, Event::EgressBitrateEstimate(BweKind::Twcc { .. })))
    );
}

fn peers(feedback: bool) -> (TestRtc, TestRtc) {
    common::init_log();
    init_crypto_default();
    let now = Instant::now();
    let l = Rtc::builder()
        .set_rtp_mode(true)
        .enable_raw_packets(true)
        .enable_bwe(Some(Bitrate::kbps(300)))
        .build(now);
    let r = Rtc::builder()
        .set_rtp_mode(true)
        .enable_raw_packets(true)
        .build(now);
    let (mut l, mut r) = connect_l_r_with_rtc(l, r);
    l.set_forced_time_advance(Duration::from_micros(100));
    r.set_forced_time_advance(Duration::from_micros(100));
    l.bwe().set_bitrate(Bitrate::ZERO, Bitrate::mbps(2));
    if feedback {
        r.direct_api().enable_twcc_feedback();
    }
    (l, r)
}

fn run(l: &mut TestRtc, r: &mut TestRtc, duration: Duration) -> Result<(), RtcError> {
    let end = l.last + duration;
    let mut iterations = 0;
    while l.last < end {
        progress(l, r)?;
        iterations += 1;
        assert!(
            iterations < 100_000,
            "empty queues must not spin on an expired timeout"
        );
    }
    Ok(())
}

fn probes(rtc: &TestRtc) -> usize {
    rtc.events
        .iter()
        .filter(|(_, event)| {
            matches!(event,
                Event::RawPacket(p) if matches!(p.as_ref(), RawPacket::RtpTx(h, _) if *h.ssrc == 0)
            )
        })
        .count()
}

fn assert_feedback_and_no_media(l: &TestRtc, r: &TestRtc) {
    assert!(probes(l) >= 5, "SSRC 0 probe cluster was not sent");
    assert!(
        l.events.iter().any(|(_, event)| matches!(event,
            Event::EgressBitrateEstimate(BweKind::Twcc { estimate: rate, can_probe: true })
            if *rate > Bitrate::kbps(300)
        )),
        "probe feedback should measure capacity above the initial estimate"
    );
    assert!(r.events.iter().any(|(_, event)| matches!(event,
        Event::RawPacket(p) if matches!(p.as_ref(), RawPacket::RtpRx(h, payload)
            if *h.ssrc == 0 && h.has_padding && h.ext_vals.transport_cc.is_some() && payload.is_empty())
    )), "receiver must decrypt and strip probe padding");
    assert!(
        !r.events
            .iter()
            .any(|(_, event)| matches!(event, Event::RtpPacket(_) | Event::MediaData(_))),
        "probes must not be delivered as media"
    );
}
