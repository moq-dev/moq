//! AMR-WB (RFC 4867) negotiation and media round-trip tests.

use std::net::Ipv4Addr;
use std::time::Duration;

use str0m::change::{SdpAnswer, SdpOffer};
use str0m::format::{Codec, FormatParams};
use str0m::media::{Direction, Frequency, MediaKind, MediaTime};
use str0m::{Event, RtcConfig};

mod common;
use common::{Peer, TestRtc, init_crypto_default, init_log, progress};

/// Speech bits of the speech (0..=8) and SID (9) frame types, RFC 4867 Table 1a.
const FRAME_BITS: [usize; 10] = [132, 177, 253, 285, 317, 365, 397, 461, 477, 40];

/// A 3GPP IF frame of `frame_type` (Q=1) whose speech bytes are `fill`, padding cleared.
fn if_frame(frame_type: usize, fill: u8) -> Vec<u8> {
    let bits = FRAME_BITS[frame_type];
    let mut frame = vec![((frame_type as u8) << 3) | 0x04];
    frame.extend(std::iter::repeat_n(fill, bits.div_ceil(8)));
    if bits % 8 != 0 {
        *frame.last_mut().unwrap() &= 0xff << (8 - bits % 8);
    }
    frame
}

/// A single AMR-WB payload type with the given layout.
fn amr_wb_config(octet_align: Option<bool>) -> impl FnOnce(RtcConfig) -> RtcConfig {
    move |c| {
        let mut c = c.clear_codecs();
        c.codec_config().add_config(
            122.into(),
            None,
            Codec::AmrWb,
            Frequency::SIXTEEN_KHZ,
            Some(1),
            FormatParams {
                octet_align,
                ..Default::default()
            },
        );
        c
    }
}

fn amr_wb_format(peer: &TestRtc) -> FormatParams {
    peer.rtc
        .codec_config()
        .find(|p| p.spec().codec == Codec::AmrWb)
        .expect("AMR-WB to be configured")
        .spec()
        .format
}

/// Sends every speech mode and SID, one 20 ms frame per write, and checks each
/// received frame and its media time.
fn round_trip(octet_align: Option<bool>) {
    init_log();
    init_crypto_default();

    let mut l = TestRtc::new_with_config(Peer::Left, amr_wb_config(octet_align));
    let mut r = TestRtc::new_with_config(Peer::Right, amr_wb_config(octet_align));
    l.add_host_candidate((Ipv4Addr::new(1, 1, 1, 1), 1000).into());
    r.add_host_candidate((Ipv4Addr::new(2, 2, 2, 2), 2000).into());

    let mut change = l.sdp_api();
    let mid = change.add_media(MediaKind::Audio, Direction::SendRecv, None, None, None);
    let (offer, pending) = change.apply().unwrap();
    let answer = r.rtc.sdp_api().accept_offer(offer).unwrap();
    l.rtc.sdp_api().accept_answer(pending, answer).unwrap();

    while !l.is_connected() || !r.is_connected() {
        progress(&mut l, &mut r).unwrap();
    }

    let mut sent = Vec::new();
    let start = l.duration();
    while l.duration() - start < Duration::from_secs(1) {
        let frame = if_frame(sent.len() % FRAME_BITS.len(), sent.len() as u8);
        let time = MediaTime::new(sent.len() as u64 * 320, Frequency::SIXTEEN_KHZ);
        let wallclock = l.start + l.duration();
        l.writer(mid)
            .unwrap()
            .write(122.into(), wallclock, time, frame.clone())
            .unwrap();
        sent.push(frame);
        progress(&mut l, &mut r).unwrap();
    }

    let received: Vec<_> = r
        .events
        .iter()
        .filter_map(|(_, e)| match e {
            Event::MediaData(d) => Some(d),
            _ => None,
        })
        .collect();

    assert!(received.len() > FRAME_BITS.len(), "{octet_align:?}");
    assert!(received.iter().any(|d| d.data[0] >> 3 == 9), "no SID frame");
    for d in received {
        assert_eq!(d.params.spec().codec, Codec::AmrWb);
        assert_eq!(d.time.frequency(), Frequency::SIXTEEN_KHZ);
        assert_eq!(d.time.numer() % 320, 0);
        let index = (d.time.numer() / 320) as usize;
        assert_eq!(d.data[..], sent[index][..], "frame {index} {octet_align:?}");
    }
}

#[test]
fn amr_wb_octet_aligned_round_trip() {
    round_trip(Some(true));
}

#[test]
fn amr_wb_bandwidth_efficient_round_trip() {
    round_trip(None);
}

#[test]
fn amr_wb_default_config_negotiates_octet_aligned() {
    init_log();
    init_crypto_default();

    let mut l = TestRtc::new_with_config(Peer::Left, |c| c.clear_codecs().enable_amr_wb(true));
    let mut r = TestRtc::new_with_config(Peer::Right, |c| c.clear_codecs().enable_amr_wb(true));

    let mut change = l.sdp_api();
    change.add_media(MediaKind::Audio, Direction::SendRecv, None, None, None);
    let (offer, pending) = change.apply().unwrap();

    let offer_sdp = offer.to_sdp_string();
    assert!(offer_sdp.contains("a=rtpmap:122 AMR-WB/16000/1\r\n"));
    assert!(offer_sdp.contains("a=fmtp:122 octet-align=1\r\n"));

    let answer = r.rtc.sdp_api().accept_offer(offer).unwrap();
    l.rtc.sdp_api().accept_answer(pending, answer).unwrap();

    for peer in [&l, &r] {
        assert_eq!(amr_wb_format(peer).octet_align, Some(true));
    }
}

/// RFC 4867 §8.3.1: the answer repeats the offer's octet-align, channels and
/// mode-set, drops parameters str0m doesn't use, and the mode-set then binds both
/// peers.
#[test]
fn amr_wb_answer_echoes_offer_and_mode_set_binds_both_sides() {
    init_log();
    init_crypto_default();

    let mut l = TestRtc::new_with_config(Peer::Left, amr_wb_config(None));
    let mut r = TestRtc::new_with_config(Peer::Right, amr_wb_config(None));

    let mut change = l.sdp_api();
    change.add_media(MediaKind::Audio, Direction::SendRecv, None, None, None);
    let (offer, pending) = change.apply().unwrap();
    let offer_sdp = offer.to_sdp_string().replace(
        "a=rtpmap:122 AMR-WB/16000/1\r\n",
        "a=rtpmap:122 AMR-WB/16000\r\n\
         a=fmtp:122 octet-align=0;mode-set=0,1,2;mode-change-capability=2;\
         max-red=0;crc=0;robust-sorting=0\r\n",
    );
    let offer = SdpOffer::from_sdp_string(&offer_sdp).unwrap();

    let answer_sdp = r.rtc.sdp_api().accept_offer(offer).unwrap().to_sdp_string();
    assert!(answer_sdp.contains("a=rtpmap:122 AMR-WB/16000\r\n"));
    assert!(answer_sdp.contains("a=fmtp:122 octet-align=0;mode-set=0,1,2\r\n"));

    let answer = SdpAnswer::from_sdp_string(&answer_sdp).unwrap();
    l.rtc.sdp_api().accept_answer(pending, answer).unwrap();

    for peer in [&l, &r] {
        assert_eq!(amr_wb_format(peer).mode_set, Some(0b111));
    }
}

#[test]
fn amr_wb_unsupported_offers_are_declined() {
    init_log();
    init_crypto_default();

    for (from, to) in [
        ("octet-align=1", "octet-align=1;crc=1"),
        ("octet-align=1", "octet-align=1;robust-sorting=1"),
        ("octet-align=1", "octet-align=1;interleaving=30"),
        ("octet-align=1", "octet-align=1;mode-change-period=2"),
        ("AMR-WB/16000/1", "AMR-WB/16000/2"),
    ] {
        let mut l = TestRtc::new_with_config(Peer::Left, |c| c.clear_codecs().enable_amr_wb(true));
        let mut r = TestRtc::new_with_config(Peer::Right, |c| c.clear_codecs().enable_amr_wb(true));

        let mut change = l.sdp_api();
        change.add_media(MediaKind::Audio, Direction::SendRecv, None, None, None);
        let (offer, _) = change.apply().unwrap();
        let offer_sdp = offer.to_sdp_string().replace(from, to);
        let offer = SdpOffer::from_sdp_string(&offer_sdp).unwrap();

        let answer_sdp = r.rtc.sdp_api().accept_offer(offer).unwrap().to_sdp_string();
        assert!(
            !answer_sdp.contains("AMR-WB"),
            "accepted `{to}`:\n{answer_sdp}"
        );
    }
}

#[test]
fn amr_wb_out_of_range_mode_set_is_ignored() {
    for mode in [9, 15, 16, 31, 32, 255] {
        assert_eq!(
            FormatParams::parse_line(&format!("mode-set={mode}")).mode_set,
            None,
            "invalid speech mode {mode}"
        );
    }
}

#[test]
fn amr_wb_invalid_mode_set_in_other_codec_sdp_does_not_panic() {
    init_crypto_default();

    let mut l = TestRtc::new_with_config(Peer::Left, |c| c);
    let mut change = l.sdp_api();
    change.add_media(MediaKind::Audio, Direction::SendRecv, None, None, None);
    let (offer, _) = change.apply().unwrap();
    let offer_sdp = offer.to_sdp_string().replace(
        "a=rtpmap:111 opus/48000/2\r\n",
        "a=rtpmap:111 opus/48000/2\r\na=fmtp:111 mode-set=16\r\n",
    );
    assert!(offer_sdp.contains("mode-set=16"));

    SdpOffer::from_sdp_string(&offer_sdp).unwrap();
}

#[test]
fn amr_wb_alternative_mode_sets_accept_one_unrestricted_remote_payload() {
    init_crypto_default();

    let mut l = TestRtc::new_with_config(Peer::Left, |c| c.clear_codecs().enable_amr_wb(true));
    let mut r = TestRtc::new_with_config(Peer::Right, |c| {
        let mut c = c.clear_codecs();
        for (pt, mode_set) in [(122, 0b111), (123, 0b111000)] {
            c.codec_config().add_config(
                pt.into(),
                None,
                Codec::AmrWb,
                Frequency::SIXTEEN_KHZ,
                Some(1),
                FormatParams {
                    octet_align: Some(true),
                    mode_set: Some(mode_set),
                    ..Default::default()
                },
            );
        }
        c
    });

    let mut change = l.sdp_api();
    change.add_media(MediaKind::Audio, Direction::SendRecv, None, None, None);
    let (offer, pending) = change.apply().unwrap();
    let answer = r.rtc.sdp_api().accept_offer(offer).unwrap();
    let answer_sdp = answer.to_sdp_string();

    // The offer has no mode-set, so RFC 4867 §8.3.1 lets the answerer choose
    // either local alternative for the one offered payload type.
    assert_eq!(answer_sdp.matches("AMR-WB/16000/1").count(), 1);
    assert!(["0,1,2", "3,4,5"].iter().any(|modes| {
        answer_sdp.contains(&format!("a=fmtp:122 octet-align=1;mode-set={modes}\r\n"))
    }));
    l.rtc.sdp_api().accept_answer(pending, answer).unwrap();
}
