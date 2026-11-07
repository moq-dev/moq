use std::net::Ipv4Addr;
use std::time::Duration;

use str0m::change::{SdpAnswer, SdpOffer, SdpPendingOffer};
use str0m::error::PacketError;
use str0m::format::{Codec, CodecConfig, CodecExtra, FormatParams};
use str0m::media::TelephoneEvent;
use str0m::media::{Direction, Frequency, MediaData, MediaKind, MediaTime, Mid, Pt};
use str0m::rtp::{RtpWrite, Ssrc};
use str0m::{Event, RtcError};

mod common;
use common::negotiate;
use common::{Peer, TestRtc, connect_l_r_with_rtc, init_crypto_default, init_log, progress};

const DIRECTIONS: [Direction; 4] = [
    Direction::SendRecv,
    Direction::SendOnly,
    Direction::RecvOnly,
    Direction::Inactive,
];

#[cfg(feature = "unversioned")]
#[test]
fn unversioned_telephone_event_wire_helpers() {
    use str0m::unversioned::TelephoneEvent as WireEvent;

    let report: TelephoneEvent = WireEvent {
        event: 5,
        end: true,
        volume: 10,
        duration: Duration::from_millis(100),
    };

    for (clock_rate, units) in [
        (Frequency::EIGHT_KHZ, 800),
        (Frequency::SIXTEEN_KHZ, 1600),
        (Frequency::new(32_000).unwrap(), 3200),
        (Frequency::FORTY_EIGHT_KHZ, 4800),
    ] {
        let bytes = report.to_bytes(clock_rate).unwrap();
        assert_eq!(&bytes[..2], &[5, 0x8a]);
        assert_eq!(u16::from_be_bytes([bytes[2], bytes[3]]), units);
        assert_eq!(WireEvent::parse(&bytes, clock_rate), Some(report));

        let packed = [bytes, bytes].concat();
        let reports: Vec<_> = WireEvent::parse_all(&packed, clock_rate).unwrap().collect();
        assert_eq!(reports, [report, report]);
        assert!(WireEvent::parse(&bytes[..3], clock_rate).is_none());
        assert!(WireEvent::parse_all(&packed[..7], clock_rate).is_none());
    }
}

fn add_events(config: &mut CodecConfig, events: &[(u8, Frequency, Option<u8>)]) {
    for &(pt, rate, max) in events {
        let format = max
            .map(|max| FormatParams::parse_line(&format!("0-{max}")))
            .unwrap_or_default();
        config.add_config(pt.into(), None, Codec::Tele, rate, None, format);
    }
}

fn with_events(peer: Peer, events: &[(u8, Frequency, Option<u8>)]) -> TestRtc {
    with_audio_events(peer, &[Codec::Opus], events)
}

fn enable_audio_codecs(config: &mut CodecConfig, codecs: &[Codec]) {
    config.enable_opus(codecs.contains(&Codec::Opus), false);
    config.enable_pcmu(codecs.contains(&Codec::PCMU), false);
    config.enable_pcma(codecs.contains(&Codec::PCMA), false);
    config.enable_g722(codecs.contains(&Codec::G722), false);
}

fn with_audio_events(
    peer: Peer,
    codecs: &[Codec],
    events: &[(u8, Frequency, Option<u8>)],
) -> TestRtc {
    TestRtc::new_with_config(peer, |config| {
        let mut config = config.clear_codecs().set_rtp_mode(true);
        let params = config.codec_config();
        enable_audio_codecs(params, codecs);
        params.enable_vp8(true);
        add_events(params, events);
        config
    })
}

fn event_params(sdp: &str) -> Vec<(u8, u32, String)> {
    SdpOffer::from_sdp_string(sdp)
        .unwrap()
        .media_lines
        .iter()
        .flat_map(|m| m.rtp_params())
        .filter(|p| p.spec().codec == Codec::Tele)
        .map(|p| {
            (
                *p.pt(),
                p.spec().clock_rate.get(),
                p.spec().format.to_string(),
            )
        })
        .collect()
}

fn event_max(rtc: &TestRtc, mid: Mid, pt: u8) -> Option<u8> {
    rtc.media(mid)
        .unwrap()
        .remote_pts()
        .contains(&pt.into())
        .then(|| {
            rtc.codec_config()
                .find(|p| p.pt() == pt.into() && p.spec().codec.is_tele())
                .map(|p| p.spec().format.telephone_event_max.unwrap_or(16))
        })
        .flatten()
}

fn offer_audio(rtc: &mut TestRtc, direction: Direction) -> (Mid, String, SdpPendingOffer) {
    let mut change = rtc.sdp_api();
    let mid = change.add_media(MediaKind::Audio, direction, None, None, None);
    let (offer, pending) = change.apply().unwrap();
    (mid, offer.to_sdp_string(), pending)
}

fn answer_offer(rtc: &mut TestRtc, offer: &str) -> String {
    rtc.sdp_api()
        .accept_offer(SdpOffer::from_sdp_string(offer).unwrap())
        .unwrap()
        .to_sdp_string()
}

fn accept_answer(rtc: &mut TestRtc, pending: SdpPendingOffer, answer: &str) {
    rtc.sdp_api()
        .accept_answer(pending, SdpAnswer::from_sdp_string(answer).unwrap())
        .unwrap();
}

fn negotiate_events(
    l: &mut TestRtc,
    r: &mut TestRtc,
    direction: Direction,
) -> (Mid, String, String) {
    let (mid, offer, pending) = offer_audio(l, direction);
    let answer = answer_offer(r, &offer);
    accept_answer(l, pending, &answer);
    (mid, offer, answer)
}

fn replace_event_fmtp(sdp: &str, value: Option<&str>) -> String {
    let mut sdp = sdp
        .lines()
        .filter(|line| !line.starts_with("a=fmtp:101 "))
        .collect::<Vec<_>>()
        .join("\r\n");
    sdp.push_str("\r\n");
    if let Some(value) = value {
        sdp.push_str(&format!("a=fmtp:101 {value}\r\n"));
    }
    sdp
}

#[test]
fn telephone_event_sdp_support_matrix() {
    init_crypto_default();
    for rate in [
        Frequency::EIGHT_KHZ,
        Frequency::SIXTEEN_KHZ,
        Frequency::FORTY_EIGHT_KHZ,
    ] {
        for direction in DIRECTIONS {
            for (offer_support, answer_support) in
                [(true, true), (true, false), (false, true), (false, false)]
            {
                let offered = [(101, rate, None)];
                let supported = [(126, rate, None)];
                let mut l = with_events(Peer::Left, if offer_support { &offered } else { &[] });
                let mut r = with_events(Peer::Right, if answer_support { &supported } else { &[] });
                let (mid, offer, answer) = negotiate_events(&mut l, &mut r, direction);
                let negotiated = offer_support && answer_support;
                assert_eq!(event_params(&offer).len(), usize::from(offer_support));
                assert_eq!(event_params(&answer).len(), usize::from(negotiated));
                assert!(!answer.contains("m=audio 0 "));
                for rtc in [&l, &r] {
                    let pts = rtc.media(mid).unwrap().remote_pts();
                    assert_eq!(pts.contains(&101.into()), negotiated);
                    assert!(pts.contains(&111.into()));
                    assert!(!pts.contains(&126.into()));
                    assert_eq!(event_max(rtc, mid, 101), negotiated.then_some(16));
                }
                if !negotiated {
                    assert!(!answer.contains("a=rtpmap:101 "));
                    assert!(!answer.contains("a=fmtp:101 "));
                }
            }
        }
    }
}

#[test]
fn telephone_event_sdp_matches_event_clock_rates() {
    init_crypto_default();
    let mut l = with_audio_events(
        Peer::Left,
        &[Codec::Opus, Codec::PCMU],
        &[
            (101, Frequency::EIGHT_KHZ, None),
            (102, Frequency::SIXTEEN_KHZ, None),
            (103, Frequency::FORTY_EIGHT_KHZ, None),
        ],
    );
    let mut r = with_audio_events(
        Peer::Right,
        &[Codec::Opus, Codec::PCMU],
        &[
            (126, Frequency::SIXTEEN_KHZ, None),
            (125, Frequency::FORTY_EIGHT_KHZ, None),
        ],
    );
    let (mid, offer, answer) = negotiate_events(&mut l, &mut r, Direction::SendRecv);
    assert_eq!(
        event_params(&offer),
        [(101, 8_000, "0-16".into()), (103, 48_000, "0-16".into())]
    );
    let rates: Vec<_> = event_params(&answer)
        .into_iter()
        .map(|(pt, rate, _)| (pt, rate))
        .collect();
    assert_eq!(rates, [(103, 48_000)]);
    for rtc in [&l, &r] {
        assert_eq!(
            rtc.media(mid).unwrap().remote_pts(),
            &[111.into(), 0.into(), 103.into()]
        );
    }

    let mut l = with_events(Peer::Left, &[(101, Frequency::EIGHT_KHZ, None)]);
    let mut r = with_events(Peer::Right, &[(126, Frequency::SIXTEEN_KHZ, None)]);
    let (_, _, answer) = negotiate_events(&mut l, &mut r, Direction::SendRecv);
    assert!(event_params(&answer).is_empty());
    assert!(!answer.contains("m=audio 0 "));
}

#[test]
fn telephone_event_sdp_prefers_audio_rtp_clock_rates() {
    init_crypto_default();
    let offered = [
        (101, Frequency::EIGHT_KHZ, None),
        (102, Frequency::SIXTEEN_KHZ, None),
        (103, Frequency::FORTY_EIGHT_KHZ, None),
    ];
    let supported = [
        (126, Frequency::EIGHT_KHZ, None),
        (125, Frequency::SIXTEEN_KHZ, None),
        (124, Frequency::FORTY_EIGHT_KHZ, None),
    ];
    let cases = [
        (&[Codec::PCMU][..], &[(101, 8_000)][..]),
        (&[Codec::PCMA][..], &[(101, 8_000)][..]),
        (&[Codec::G722][..], &[(101, 8_000)][..]),
        (&[Codec::Opus][..], &[(103, 48_000)][..]),
        (&[Codec::PCMU, Codec::PCMA][..], &[(101, 8_000)][..]),
        (
            &[Codec::Opus, Codec::PCMU][..],
            &[(101, 8_000), (103, 48_000)][..],
        ),
        (
            &[Codec::Opus, Codec::PCMA][..],
            &[(101, 8_000), (103, 48_000)][..],
        ),
        (
            &[Codec::Opus, Codec::G722][..],
            &[(101, 8_000), (103, 48_000)][..],
        ),
    ];

    for (codecs, expected) in cases {
        let expected: Vec<_> = expected
            .iter()
            .map(|&(pt, rate)| (pt, rate, "0-16".to_string()))
            .collect();
        for direction in DIRECTIONS {
            let mut l = with_audio_events(Peer::Left, codecs, &offered);
            let mut r = with_audio_events(Peer::Right, codecs, &supported);
            let (mid, offer, answer) = negotiate_events(&mut l, &mut r, direction);
            for sdp in [&offer, &answer] {
                assert_eq!(event_params(sdp), expected, "{codecs:?} {direction:?}");
                if codecs.contains(&Codec::G722) {
                    assert!(sdp.contains("a=rtpmap:9 G722/8000\r\n"), "{sdp}");
                    assert!(!sdp.contains("G722/16000"), "{sdp}");
                }
            }
            for rtc in [&l, &r] {
                let media = rtc.media(mid).unwrap();
                for &(pt, _, _) in &offered {
                    let negotiated = expected.iter().any(|p| p.0 == pt);
                    assert_eq!(media.remote_pts().contains(&pt.into()), negotiated);
                    assert_eq!(event_max(rtc, mid, pt), negotiated.then_some(16));
                }
                assert_eq!(
                    rtc.codec_config()
                        .iter()
                        .filter(|p| p.spec().codec == Codec::Tele)
                        .count(),
                    3
                );
            }
        }
    }
}

#[test]
fn telephone_event_sdp_accepts_8khz_with_opus() {
    init_crypto_default();
    for direction in DIRECTIONS {
        let mut l = with_events(Peer::Left, &[(101, Frequency::EIGHT_KHZ, None)]);
        let mut r = with_events(
            Peer::Right,
            &[
                (126, Frequency::EIGHT_KHZ, Some(7)),
                (125, Frequency::FORTY_EIGHT_KHZ, None),
            ],
        );
        let (mid, offer, answer) = negotiate_events(&mut l, &mut r, direction);
        assert_eq!(event_params(&offer), [(101, 8_000, "0-16".into())]);
        assert_eq!(event_params(&answer), [(101, 8_000, "0-7".into())]);
        for rtc in [&l, &r] {
            let media = rtc.media(mid).unwrap();
            assert_eq!(media.remote_pts(), &[111.into(), 101.into()]);
            assert_eq!(event_max(&l, mid, 101), Some(7));
        }
    }
}

#[test]
fn telephone_event_sdp_reoffer_preserves_negotiated_fallback() {
    init_crypto_default();
    let mut l = with_events(Peer::Left, &[(101, Frequency::EIGHT_KHZ, None)]);
    let mut r = with_events(
        Peer::Right,
        &[
            (126, Frequency::EIGHT_KHZ, None),
            (125, Frequency::FORTY_EIGHT_KHZ, None),
        ],
    );
    let (mid, _, _) = negotiate_events(&mut l, &mut r, Direction::SendRecv);

    let (new_mid, offer, pending) = offer_audio(&mut r, Direction::SendRecv);
    assert_eq!(
        event_params(&offer),
        [(101, 8_000, "0-16".into()), (125, 48_000, "0-16".into())]
    );
    let answer = answer_offer(&mut l, &offer);
    assert_eq!(event_params(&answer), [(101, 8_000, "0-16".into())]);
    accept_answer(&mut r, pending, &answer);
    for rtc in [&l, &r] {
        assert_eq!(event_max(rtc, mid, 101), Some(16));
        assert_eq!(rtc.media(new_mid).unwrap().remote_pts(), &[111.into()]);
    }
}

#[test]
fn telephone_event_sdp_ignores_unlisted_payloads() {
    init_crypto_default();
    let mut l = with_events(Peer::Left, &[(101, Frequency::FORTY_EIGHT_KHZ, None)]);
    let mut r = with_events(Peer::Right, &[(126, Frequency::FORTY_EIGHT_KHZ, None)]);
    let (mid, offer, pending) = offer_audio(&mut l, Direction::SendRecv);
    // The m-line, not a leftover rtpmap/fmtp, determines which payloads are offered.
    let offer = offer.replace("SAVPF 111 101\r\n", "SAVPF 111\r\n");
    let answer = answer_offer(&mut r, &offer);
    assert!(event_params(&answer).is_empty());
    accept_answer(&mut l, pending, &answer);

    for rtc in [&l, &r] {
        assert_eq!(rtc.media(mid).unwrap().remote_pts(), &[111.into()]);
        assert_eq!(event_max(rtc, mid, 101), None);
    }
}

#[test]
fn telephone_event_sdp_event_ranges() {
    init_crypto_default();
    for direction in DIRECTIONS {
        for (offered, supported, expected) in [
            (None, None, 16),
            (Some(15), None, 15),
            (None, Some(15), 15),
            (Some(16), None, 16),
            (None, Some(16), 16),
            (Some(16), Some(16), 16),
            (Some(16), Some(7), 7),
            (Some(7), Some(16), 7),
            (Some(0), Some(16), 0),
            (Some(255), Some(255), 255),
        ] {
            let mut l = with_events(Peer::Left, &[(101, Frequency::FORTY_EIGHT_KHZ, offered)]);
            let mut r = with_events(Peer::Right, &[(126, Frequency::FORTY_EIGHT_KHZ, supported)]);
            let (mid, offer, answer) = negotiate_events(&mut l, &mut r, direction);
            assert!(offer.contains(&format!("a=fmtp:101 0-{}\r\n", offered.unwrap_or(16))));
            assert!(answer.contains(&format!("a=fmtp:101 0-{expected}\r\n")));
            assert_eq!(
                event_params(&answer),
                [(101, 48_000, format!("0-{expected}"))]
            );
            for rtc in [&l, &r] {
                assert_eq!(event_max(rtc, mid, 101), Some(expected));
            }
        }
    }
}

#[test]
fn telephone_event_sdp_missing_or_invalid_fmtp() {
    init_crypto_default();
    for (fmtp, expected) in [
        (None, Some(16)),
        (Some("0-15"), Some(15)),
        (Some("0-16"), Some(16)),
        (Some("0-255"), Some(16)),
        (Some("0-256"), None),
        (Some("15-0"), None),
        (Some("1-15"), None),
        (Some("0-15,66,70"), None),
        (Some("0-+15"), None),
        (Some(""), None),
        (Some("bogus"), None),
        (Some("0-15 16"), None),
        (Some("0-15;useinbandfec=1"), None),
        (Some("0-15\r\na=fmtp:101 0-16"), None),
    ] {
        let mut l = with_events(Peer::Left, &[(101, Frequency::FORTY_EIGHT_KHZ, None)]);
        let mut r = with_events(Peer::Right, &[(126, Frequency::FORTY_EIGHT_KHZ, None)]);
        let (mid, offer, pending) = offer_audio(&mut l, Direction::SendRecv);
        let offer = replace_event_fmtp(&offer, fmtp);
        let answer = answer_offer(&mut r, &offer);
        assert_eq!(
            event_params(&answer).len(),
            usize::from(expected.is_some()),
            "{fmtp:?}"
        );
        if let Some(max) = expected {
            assert!(
                answer.contains(&format!("a=fmtp:101 0-{max}\r\n")),
                "{fmtp:?}"
            );
        }
        assert!(!answer.contains("m=audio 0 "));
        accept_answer(&mut l, pending, &answer);
        for rtc in [&l, &r] {
            assert_eq!(
                rtc.media(mid).unwrap().remote_pts().contains(&101.into()),
                expected.is_some()
            );
            assert_eq!(event_max(rtc, mid, 101), expected);
        }
    }
}

#[test]
fn bare_event_range_requires_telephone_event_rtpmap() {
    init_crypto_default();
    let mut rtc = with_events(Peer::Left, &[(101, Frequency::FORTY_EIGHT_KHZ, None)]);
    let (_, offer, _) = offer_audio(&mut rtc, Direction::SendRecv);
    let offer = offer.replace(
        "a=rtpmap:101 telephone-event/48000",
        "a=rtpmap:101 opus/48000/2",
    );
    let parsed = SdpOffer::from_sdp_string(&offer).unwrap();
    let params = parsed.media_lines[0].rtp_params();
    let other = params.iter().find(|p| p.pt() == 101.into()).unwrap();
    assert_eq!(other.spec().codec, Codec::Opus);
    assert_eq!(other.spec().format.telephone_event_max, None);
}

#[test]
fn telephone_event_sdp_answer_fmtp() {
    init_crypto_default();
    for (fmtp, expected) in [
        (None, Some(16)),
        (Some("0-7"), Some(7)),
        (Some("0-15"), Some(15)),
        (Some("0-16"), Some(16)),
        (Some("0-255"), Some(16)),
        (Some("0-15,66,70"), None),
    ] {
        let mut l = with_events(Peer::Left, &[(101, Frequency::FORTY_EIGHT_KHZ, None)]);
        let mut r = with_events(Peer::Right, &[(126, Frequency::FORTY_EIGHT_KHZ, None)]);
        let (mid, offer, pending) = offer_audio(&mut l, Direction::SendRecv);
        let answer = answer_offer(&mut r, &offer);
        let answer = replace_event_fmtp(&answer, fmtp);
        accept_answer(&mut l, pending, &answer);
        let media = l.media(mid).unwrap();
        assert_eq!(event_max(&l, mid, 101), expected, "{fmtp:?}");
        assert_eq!(media.remote_pts().contains(&101.into()), expected.is_some());
        assert!(media.remote_pts().contains(&111.into()));
    }
}

#[test]
fn telephone_event_sdp_ranges_are_per_pt_across_media() {
    init_crypto_default();
    let mut l = with_events(Peer::Left, &[(101, Frequency::FORTY_EIGHT_KHZ, Some(16))]);
    let mut r = with_events(Peer::Right, &[(126, Frequency::FORTY_EIGHT_KHZ, Some(16))]);
    let mut change = l.sdp_api();
    let mid1 = change.add_media(MediaKind::Audio, Direction::SendRecv, None, None, None);
    let mid2 = change.add_media(MediaKind::Audio, Direction::SendRecv, None, None, None);
    change.add_media(MediaKind::Video, Direction::SendRecv, None, None, None);
    let (offer, pending) = change.apply().unwrap();
    let offer = offer
        .to_sdp_string()
        .replacen("a=fmtp:101 0-16", "a=fmtp:101 0-7", 1);
    let answer = answer_offer(&mut r, &offer);
    assert_eq!(
        event_params(&answer),
        [(101, 48_000, "0-7".into()), (101, 48_000, "0-7".into())]
    );
    accept_answer(&mut l, pending, &answer);
    for rtc in [&l, &r] {
        for mid in [mid1, mid2] {
            assert_eq!(event_max(rtc, mid, 101), Some(7));
        }
        let params = rtc
            .codec_config()
            .find(|p| p.spec().codec == Codec::Tele)
            .unwrap();
        assert_eq!(params.spec().format.telephone_event_max, Some(7));
    }
}

#[test]
fn telephone_event_rtp_roundtrip() -> Result<(), RtcError> {
    init_log();
    init_crypto_default();

    for (audio_codec, audio_clock_rate, event_clock_rate) in [
        (Codec::PCMU, Frequency::EIGHT_KHZ, Frequency::EIGHT_KHZ),
        (Codec::PCMA, Frequency::EIGHT_KHZ, Frequency::EIGHT_KHZ),
        (Codec::G722, Frequency::EIGHT_KHZ, Frequency::EIGHT_KHZ),
        (
            Codec::Opus,
            Frequency::FORTY_EIGHT_KHZ,
            Frequency::FORTY_EIGHT_KHZ,
        ),
        (
            Codec::Opus,
            Frequency::FORTY_EIGHT_KHZ,
            Frequency::EIGHT_KHZ,
        ),
    ] {
        let configured = |peer, pt, channels| {
            TestRtc::new_with_config(peer, |config| {
                let mut config = config.clear_codecs().set_rtp_mode(true);
                enable_audio_codecs(config.codec_config(), &[audio_codec]);
                config.codec_config().add_config(
                    pt,
                    None,
                    Codec::Tele,
                    event_clock_rate,
                    channels,
                    FormatParams::default(),
                );
                config
            })
        };
        let mut l = configured(Peer::Left, 101.into(), None);
        let mut r = configured(Peer::Right, 126.into(), Some(1));
        l.add_host_candidate((Ipv4Addr::new(1, 1, 1, 1), 1000).into());
        r.add_host_candidate((Ipv4Addr::new(2, 2, 2, 2), 2000).into());

        let (mid, offer, answer) = negotiate_events(&mut l, &mut r, Direction::SendRecv);
        let rtpmap = format!("a=rtpmap:101 telephone-event/{}", event_clock_rate.get());
        assert!(offer.contains(&rtpmap), "{offer}");
        assert!(offer.contains("a=fmtp:101 0-16\r\n"));
        assert!(answer.contains(&rtpmap), "{answer}");

        for rtc in [&l, &r] {
            let params = rtc
                .codec_config()
                .find(|p| p.spec().codec == Codec::Tele)
                .unwrap();
            assert_eq!(*params.pt(), 101);
            assert_eq!(params.spec().clock_rate, event_clock_rate);
        }

        let audio_pt = *l
            .codec_config()
            .find(|p| p.spec().codec == audio_codec)
            .unwrap()
            .pt();
        let audio_timestamp = 4960 * audio_clock_rate.get() / event_clock_rate.get();
        let packets: &[(u8, u32, bool, &[u8])] = &[
            (audio_pt, 0, false, &[1, 2, 3, 4]),
            (101, 160, true, &[5, 0x0a, 0x00, 0xa0]),
            (101, 160, false, &[5, 0x0a, 0x01, 0x40]),
            (101, 160, false, &[5, 0x8a, 0x12, 0xc0]),
            (101, 160, false, &[5, 0x8a, 0x12, 0xc0]),
            (101, 160, false, &[5, 0x8a, 0x12, 0xc0]),
            (audio_pt, audio_timestamp, false, &[5, 6, 7, 8]),
        ];

        assert_rtp_roundtrip(
            &mut l,
            &mut r,
            mid,
            packets,
            &[(audio_pt, audio_clock_rate), (101, event_clock_rate)],
        )?;
    }

    Ok(())
}

#[test]
fn telephone_event_rtp_direct_api_roundtrip() -> Result<(), RtcError> {
    init_log();
    init_crypto_default();

    let events = [
        (101, Frequency::EIGHT_KHZ, None),
        (110, Frequency::FORTY_EIGHT_KHZ, None),
    ];
    let l = with_events(Peer::Left, &events);
    let r = with_events(Peer::Right, &events);
    let (mut l, mut r) = connect_l_r_with_rtc(l.rtc, r.rtc);

    let mid: Mid = "audio".into();
    let ssrc_l: Ssrc = 1.into();
    let ssrc_r: Ssrc = 2.into();
    for (rtc, ssrc_tx, ssrc_rx) in [(&mut l, ssrc_l, ssrc_r), (&mut r, ssrc_r, ssrc_l)] {
        let mut direct = rtc.direct_api();
        direct.declare_media(mid, MediaKind::Audio);
        direct.declare_stream_tx(ssrc_tx, None, mid, None);
        direct.expect_stream_rx(ssrc_rx, None, mid, None);

        let media = rtc.media(mid).unwrap();
        assert!(media.remote_pts().is_empty());
        for pt in [101, 110] {
            assert_eq!(event_max(rtc, mid, pt), None);
        }
    }

    let packets: &[(u8, u32, bool, &[u8])] = &[
        (111, 0, false, &[1, 2, 3, 4]),
        (101, 160, true, &[16, 0x0a, 0x00, 0xa0]),
        (101, 160, false, &[16, 0x0a, 0x01, 0x40]),
        (101, 160, false, &[16, 0x8a, 0x12, 0xc0]),
        (101, 160, false, &[16, 0x8a, 0x12, 0xc0]),
        (101, 160, false, &[16, 0x8a, 0x12, 0xc0]),
        (111, 29_760, false, &[5, 6, 7, 8]),
        (110, 30_720, true, &[5, 0x0a, 0x03, 0xc0]),
        (110, 30_720, false, &[5, 0x0a, 0x07, 0x80]),
        (110, 30_720, false, &[5, 0x8a, 0x12, 0xc0]),
        (110, 30_720, false, &[5, 0x8a, 0x12, 0xc0]),
        (110, 30_720, false, &[5, 0x8a, 0x12, 0xc0]),
        (111, 35_520, false, &[9, 10, 11, 12]),
    ];
    assert_rtp_roundtrip(
        &mut l,
        &mut r,
        mid,
        packets,
        &[
            (111, Frequency::FORTY_EIGHT_KHZ),
            (101, Frequency::EIGHT_KHZ),
            (110, Frequency::FORTY_EIGHT_KHZ),
        ],
    )
}

fn assert_rtp_roundtrip(
    l: &mut TestRtc,
    r: &mut TestRtc,
    mid: Mid,
    packets: &[(u8, u32, bool, &[u8])],
    clock_rates: &[(u8, Frequency)],
) -> Result<(), RtcError> {
    while !l.is_connected() || !r.is_connected() {
        assert!(
            l.duration() < Duration::from_secs(5),
            "connection timed out"
        );
        progress(l, r)?;
    }
    let max = l.last.max(r.last);
    l.last = max;
    r.last = max;

    for reverse in [false, true] {
        let (tx, rx) = if reverse {
            (&mut *r, &mut *l)
        } else {
            (&mut *l, &mut *r)
        };
        let ssrc = tx.direct_api().stream_tx_by_mid(mid, None).unwrap().ssrc();
        for (index, &(pt, timestamp, marker, payload)) in packets.iter().enumerate() {
            let wallclock = tx.start + tx.duration();
            tx.direct_api().stream_tx(&ssrc).unwrap().write_rtp(
                RtpWrite::new(
                    pt.into(),
                    (index as u64 + 1).into(),
                    timestamp,
                    wallclock,
                    payload,
                )
                .marker(marker),
            );
            progress(tx, rx)?;
        }

        let deadline = tx.duration() + Duration::from_secs(1);
        while tx.duration() < deadline {
            progress(tx, rx)?;
        }

        let received: Vec<_> = rx
            .events
            .iter()
            .filter_map(|(_, event)| match event {
                Event::RtpPacket(packet) => Some(packet),
                _ => None,
            })
            .collect();
        assert_eq!(received.len(), packets.len());
        for (index, (packet, &(pt, timestamp, marker, payload))) in
            received.iter().zip(packets).enumerate()
        {
            assert_eq!(packet.header.ssrc, ssrc);
            assert_eq!(*packet.header.payload_type, pt);
            assert_eq!(*packet.seq_no, index as u64 + 1);
            assert_eq!(packet.header.timestamp, timestamp);
            assert_eq!(packet.header.marker, marker);
            let clock_rate = clock_rates
                .iter()
                .find_map(|&(payload_type, rate)| (payload_type == pt).then_some(rate))
                .expect("expected clock rate for payload type");
            assert_eq!(packet.time.frequency(), clock_rate);
            assert_eq!(packet.time.numer(), u64::from(timestamp));
            assert_eq!(packet.payload.as_ref(), payload);
        }
    }

    Ok(())
}

#[test]
fn telephone_event_frame_roundtrip() -> Result<(), RtcError> {
    init_log();
    init_crypto_default();

    let (mut l, mut r, mid) = connected_frame_mode();
    let audio_pt = l.params_opus().pt();
    let start = MediaTime::new(960, Frequency::FORTY_EIGHT_KHZ);
    let report = |end, duration: u64| TelephoneEvent {
        event: 5,
        end,
        volume: 10,
        duration: Duration::from_millis(duration / 48),
    };
    // An update every 20 ms, then the final report three times when the event ends. Like
    // libwebrtc, each report is its own packet, so the three finals go out back-to-back.
    let reports = [
        (20, report(false, 960)),
        (40, report(false, 1920)),
        (60, report(false, 2880)),
        (80, report(false, 3840)),
        (100, report(false, 4800)),
        (110, report(true, 5280)),
        (110, report(true, 5280)),
        (110, report(true, 5280)),
    ];

    write_audio(&mut l, mid, audio_pt, 0)?;
    let written_at = l.last;
    l.writer(mid)
        .unwrap()
        .telephone_event(tele(5, Duration::from_millis(110), 10))
        .write(audio_pt, written_at, start, [])?;
    advance_with_empty_audio(&mut l, &mut r, mid, audio_pt, written_at, start, 110)?;
    write_audio(&mut l, mid, audio_pt, 9600)?;
    progress_for(&mut l, &mut r, Duration::from_secs(1))?;

    let events = telephone_events(&r);
    assert_eq!(events.len(), reports.len());
    let mut previous_seq = None;
    for (index, (data, (_, report))) in events.into_iter().zip(reports).enumerate() {
        assert_eq!(
            data.data.as_ref(),
            report.to_bytes(Frequency::FORTY_EIGHT_KHZ).unwrap()
        );
        assert_eq!(data.codec_extra, CodecExtra::Tele(report));
        assert_eq!(data.audio_start_of_talk_spurt, index == 0);
        assert!(!data.is_keyframe());
        assert_eq!(data.time.numer(), start.numer());
        assert!(data.network_time >= written_at);
        assert_eq!(data.seq_range.start(), data.seq_range.end());
        if let Some(previous) = previous_seq {
            assert!(**data.seq_range.start() > previous);
        }
        previous_seq = Some(**data.seq_range.start());
    }

    let audio = r
        .events
        .iter()
        .filter(|(_, event)| {
            matches!(event, Event::MediaData(data) if data.params.spec().codec == Codec::Opus)
        })
        .count();
    assert_eq!(audio, 2, "telephone events must not stall the audio");
    Ok(())
}

#[test]
fn telephone_event_frame_reports_take_sequence_numbers_when_sent() -> Result<(), RtcError> {
    init_log();
    init_crypto_default();

    let (mut l, mut r, mid) = connected_frame_mode();
    let audio_pt = l.params_opus().pt();
    let written_at = l.last;
    l.writer(mid)
        .unwrap()
        .telephone_event(tele(7, Duration::from_millis(250), 10))
        .write(
            audio_pt,
            written_at,
            MediaTime::new(0, Frequency::FORTY_EIGHT_KHZ),
            [0xf8, 0xff, 0xfe],
        )?;
    // Audio every 20 ms while the event is sent.
    let mut frames = 1;
    while l.last < written_at + Duration::from_millis(300) {
        if l.last >= written_at + Duration::from_millis(20) * frames {
            write_audio(&mut l, mid, audio_pt, 960 * u64::from(frames))?;
            frames += 1;
        }
        progress(&mut l, &mut r)?;
    }
    progress_for(&mut l, &mut r, Duration::from_millis(500))?;

    // The audio and the reports share one series of sequence numbers. Each report takes the
    // next one when it is sent, so audio sits between them.
    let mut received: Vec<_> = r
        .events
        .iter()
        .filter_map(|(_, event)| match event {
            Event::MediaData(data) => {
                Some((**data.seq_range.start(), data.params.spec().codec.is_tele()))
            }
            _ => None,
        })
        .collect();
    received.sort();
    assert!(
        received.windows(2).all(|p| p[0].0 + 1 == p[1].0),
        "{received:?}"
    );
    let first = received.iter().position(|p| p.1).unwrap();
    let last = received.iter().rposition(|p| p.1).unwrap();
    let audio = received[first..last].iter().filter(|p| !p.1).count();
    assert!(audio >= 10, "{received:?}");
    Ok(())
}

#[test]
fn telephone_event_packets_send_on_deadlines_with_empty_audio() -> Result<(), RtcError> {
    init_crypto_default();

    let (mut l, mut r, mid) = connected_frame_mode();
    let audio_pt = l.params_opus().pt();
    let start = l.last;
    let rtp_start = MediaTime::new(0, Frequency::FORTY_EIGHT_KHZ);
    l.writer(mid)
        .unwrap()
        .telephone_event(tele(5, Duration::from_millis(100), 10))
        .write(audio_pt, start, rtp_start, [])?;

    progress_for(&mut l, &mut r, Duration::from_millis(200))?;
    let events = telephone_events(&r);
    assert_eq!(events.len(), 7);
    assert!(
        events[..4]
            .iter()
            .all(|data| { matches!(data.codec_extra, CodecExtra::Tele(value) if !value.end) })
    );
    assert!(events[4..].iter().all(|data| {
        data.codec_extra
            == CodecExtra::Tele(TelephoneEvent {
                event: 5,
                end: true,
                volume: 10,
                duration: Duration::from_millis(100),
            })
    }));
    assert!(r.events.iter().all(|(_, event)| {
        !matches!(event, Event::MediaData(data) if data.params.spec().codec == Codec::Opus)
    }));
    Ok(())
}

#[test]
fn audio_written_out_of_order_is_inserted_among_telephone_packets() -> Result<(), RtcError> {
    init_crypto_default();

    let (mut l, mut r, mid) = connected_frame_mode();
    let audio_pt = l.params_opus().pt();
    let start = l.last;
    l.writer(mid)
        .unwrap()
        .telephone_event(tele(5, Duration::from_millis(100), 10))
        .write(
            audio_pt,
            start,
            MediaTime::new(0, Frequency::FORTY_EIGHT_KHZ),
            [],
        )?;
    for millis in [30, 10] {
        let wallclock = start + Duration::from_millis(millis);
        let rtp_time = MediaTime::new(millis * 48, Frequency::FORTY_EIGHT_KHZ);
        l.writer(mid)
            .unwrap()
            .write(audio_pt, wallclock, rtp_time, [0xf8, 0xff, 0xfe])?;
    }
    progress_for(&mut l, &mut r, Duration::from_millis(200))?;

    let mut packets: Vec<_> = r
        .events
        .iter()
        .filter_map(|(_, event)| match event {
            Event::MediaData(data) => {
                Some((**data.seq_range.start(), data.params.spec().codec.is_tele()))
            }
            _ => None,
        })
        .collect();
    packets.sort_by_key(|p| p.0);
    assert_eq!(
        &packets[..4].iter().map(|p| p.1).collect::<Vec<_>>(),
        &[false, true, false, true]
    );
    Ok(())
}

#[test]
fn telephone_event_frame_long_duration_roundtrip() -> Result<(), RtcError> {
    init_crypto_default();

    let (mut l, mut r, mid) = connected_frame_mode();
    let start = MediaTime::new(0, Frequency::FORTY_EIGHT_KHZ);
    let wallclock = l.last;
    let audio_pt = l.params_opus().pt();
    l.writer(mid)
        .unwrap()
        .telephone_event(tele(5, Duration::from_secs(3), 10))
        .write(audio_pt, wallclock, start, [])?;
    advance_with_empty_audio(&mut l, &mut r, mid, audio_pt, wallclock, start, 3000)?;
    progress_for(&mut l, &mut r, Duration::from_secs(1))?;

    let events = telephone_events(&r);
    assert_eq!(events.len(), 154);
    let CodecExtra::Tele(last) = &events.last().unwrap().codec_extra else {
        panic!("expected telephone-event report");
    };
    assert_eq!(last.duration, Duration::from_micros(269_375));
    assert!(last.end);
    Ok(())
}

#[test]
fn maximum_duration_telephone_event_fits_payload_queue() -> Result<(), RtcError> {
    init_crypto_default();

    let (mut l, _r, mid) = connected_frame_mode();
    let audio_pt = l.params_opus().pt();
    let wallclock = l.last;
    let rtp_time = MediaTime::new(0, Frequency::FORTY_EIGHT_KHZ);
    l.writer(mid)
        .unwrap()
        .telephone_event(tele(5, Duration::from_secs(6), 10))
        .write(audio_pt, wallclock, rtp_time, [])
}

#[test]
fn telephone_event_frame_queued_events_wait_for_each_other() -> Result<(), RtcError> {
    init_log();
    init_crypto_default();

    let (mut l, mut r, mid) = connected_frame_mode();
    let written_at = l.last;
    let start = MediaTime::new(48_000, Frequency::FORTY_EIGHT_KHZ);

    let audio_pt = l.params_opus().pt();
    // Dial "12#" with 50 ms pauses between digits.
    for (index, event) in [1, 2, 11].into_iter().enumerate() {
        let offset = 150 * index as u64;
        let event_wallclock = written_at + Duration::from_millis(offset);
        let event_time = MediaTime::new(start.numer() + offset * 48, start.frequency());
        l.writer(mid)
            .unwrap()
            .telephone_event(tele(event, Duration::from_millis(100), 10))
            .write(audio_pt, event_wallclock, event_time, [])?;
        advance_with_empty_audio(
            &mut l,
            &mut r,
            mid,
            audio_pt,
            event_wallclock,
            event_time,
            150,
        )?;
    }
    progress_for(&mut l, &mut r, Duration::from_millis(100))?;

    let events = telephone_events(&r);
    assert_eq!(events.len(), 3 * 7);
    for (index, (digit, reports)) in [1, 2, 11].into_iter().zip(events.chunks(7)).enumerate() {
        // Waiting moves the start of a digit in wallclock and RTP time alike.
        let offset = 150 * index as u64;
        for (report_index, data) in reports.iter().enumerate() {
            // Updates at 20 to 80 ms, then the final reports at 100 ms.
            let ticks = (report_index as u64 + 1).min(5);
            let expected = TelephoneEvent {
                event: digit,
                end: ticks == 5,
                volume: 10,
                duration: Duration::from_millis(20 * ticks),
            };
            assert_eq!(data.codec_extra, CodecExtra::Tele(expected));
            assert_eq!(data.audio_start_of_talk_spurt, report_index == 0);
            assert_eq!(data.time.numer(), start.numer() + offset * 48);
            assert!(data.network_time >= written_at);
        }
    }
    Ok(())
}

#[test]
fn telephone_event_frame_stops_when_media_stops_sending() -> Result<(), RtcError> {
    init_log();
    init_crypto_default();

    let (mut l, mut r, mid) = connected_frame_mode();
    let start = MediaTime::new(960, Frequency::FORTY_EIGHT_KHZ);
    let audio_pt = l.params_opus().pt();
    let write = |rtc: &mut TestRtc, event| {
        let wallclock = rtc.last;
        rtc.writer(mid)
            .unwrap()
            .telephone_event(tele(event, Duration::from_millis(500), 10))
            .write(audio_pt, wallclock, start, [])
    };
    write(&mut l, 1)?;
    assert!(matches!(
        write(&mut l, 2),
        Err(RtcError::Packet(
            _,
            _,
            PacketError::InvalidTelephoneEvent("telephone events must be at least 50 ms apart")
        ))
    ));
    // Advance past the 100 ms update deadline.
    let first = l.last;
    advance_with_empty_audio(&mut l, &mut r, mid, audio_pt, first, start, 120)?;
    let sent_before_direction_change = telephone_events(&r).len();
    assert!(sent_before_direction_change >= 5);

    negotiate(&mut l, &mut r, |change| {
        change.set_direction(mid, Direction::RecvOnly)
    });
    assert!(matches!(
        write(&mut l, 3),
        Err(RtcError::NotSendingDirection(Direction::RecvOnly))
    ));

    // Sending again does not resume the dropped events.
    negotiate(&mut l, &mut r, |change| {
        change.set_direction(mid, Direction::SendRecv)
    });
    progress_for(&mut l, &mut r, Duration::from_secs(2))?;
    assert_eq!(telephone_events(&r).len(), sent_before_direction_change);
    Ok(())
}

#[test]
fn telephone_event_frame_packed_reports_arrive_as_separate_samples() -> Result<(), RtcError> {
    init_log();
    init_crypto_default();

    let (mut l, mut r, mid) = connected_frame_mode();
    let first = TelephoneEvent {
        event: 1,
        end: true,
        volume: 10,
        duration: Duration::from_millis(20),
    };
    let second = TelephoneEvent {
        event: 2,
        end: true,
        volume: 10,
        duration: Duration::from_millis(10),
    };
    let packed = [
        first.to_bytes(Frequency::FORTY_EIGHT_KHZ).unwrap(),
        second.to_bytes(Frequency::FORTY_EIGHT_KHZ).unwrap(),
    ]
    .concat();

    let wallclock = l.start + l.duration();
    let start = MediaTime::new(960, Frequency::FORTY_EIGHT_KHZ);
    l.writer(mid)
        .unwrap()
        .write(EVENT_PT.into(), wallclock, start, packed.clone())?;
    progress_for(&mut l, &mut r, Duration::from_secs(1))?;

    let events = telephone_events(&r);
    assert_eq!(events.len(), 2);
    assert_eq!(events[0].data.as_ref(), &packed[..4]);
    assert_eq!(events[1].data.as_ref(), &packed[4..]);
    assert_eq!(events[0].codec_extra, CodecExtra::Tele(first));
    assert_eq!(events[1].codec_extra, CodecExtra::Tele(second));
    assert_eq!(events[0].time, start);
    assert_eq!(events[1].time.numer(), start.numer() + 960);
    assert_eq!(events[0].seq_range, events[1].seq_range);
    // A directly written telephone payload does not set the RTP marker bit.
    assert!(!events[0].audio_start_of_talk_spurt);
    assert!(!events[1].audio_start_of_talk_spurt);
    Ok(())
}

#[test]
fn telephone_event_frame_reports_malformed_payloads() -> Result<(), RtcError> {
    init_log();
    init_crypto_default();

    let (mut l, mut r, mid) = connected_frame_mode();
    let report = TelephoneEvent {
        event: 1,
        end: false,
        volume: 10,
        duration: Duration::from_millis(20),
    };
    let start = MediaTime::new(960, Frequency::FORTY_EIGHT_KHZ);

    let wallclock = l.start + l.duration();
    l.writer(mid)
        .unwrap()
        .write(EVENT_PT.into(), wallclock, start, [1, 2, 3])?;
    let result = progress_for(&mut l, &mut r, Duration::from_secs(1));
    assert!(matches!(
        result,
        Err(RtcError::Packet(m, pt, PacketError::InvalidTelephoneEvent(reason)))
            if m == mid && pt == EVENT_PT.into()
                && reason == "payload must contain one or more complete 4-byte reports"
    ));
    assert!(telephone_events(&r).is_empty());

    let wallclock = l.start + l.duration();
    l.writer(mid).unwrap().write(
        EVENT_PT.into(),
        wallclock,
        start,
        report.to_bytes(Frequency::FORTY_EIGHT_KHZ).unwrap(),
    )?;
    progress_for(&mut l, &mut r, Duration::from_secs(1))?;

    let events = telephone_events(&r);
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].codec_extra, CodecExtra::Tele(report));
    Ok(())
}

#[test]
fn telephone_event_write_rejects_unnegotiated_clock_rate() {
    init_crypto_default();
    let expected_reason = "no negotiated telephone event for audio clock rate";

    let events = [(EVENT_PT, Frequency::FORTY_EIGHT_KHZ, None)];
    for (offer_support, answer_support) in [(true, false), (false, true)] {
        let mut l = with_frame_events(Peer::Left, if offer_support { &events } else { &[] });
        let mut r = with_frame_events(Peer::Right, if answer_support { &events } else { &[] });
        let (mid, _, _) = negotiate_events(&mut l, &mut r, Direction::SendRecv);
        let rtc = if offer_support { &mut l } else { &mut r };
        let pt = EVENT_PT.into();
        assert!(
            rtc.codec_config()
                .find(|p| p.pt() == pt && p.spec().codec.is_tele())
                .is_some()
        );
        assert!(!rtc.media(mid).unwrap().remote_pts().contains(&pt));

        let wallclock = rtc.last;
        let start = MediaTime::new(0, Frequency::FORTY_EIGHT_KHZ);
        let audio_pt = rtc.params_opus().pt();
        let result = rtc
            .writer(mid)
            .unwrap()
            .telephone_event(tele(5, Duration::from_millis(100), 10))
            .write(audio_pt, wallclock, start, []);
        assert!(matches!(
            result,
            Err(RtcError::Packet(_, p, PacketError::InvalidTelephoneEvent(reason)))
                if p == audio_pt && reason == expected_reason
        ));
    }

    // A negotiated 8 kHz telephone-event PT cannot share Opus's 48 kHz RTP stream.
    let events = [(EVENT_PT, Frequency::EIGHT_KHZ, None)];
    let mut l = with_frame_events(Peer::Left, &events);
    let mut r = with_frame_events(Peer::Right, &events);
    let (mid, _, _) = negotiate_events(&mut l, &mut r, Direction::SendRecv);
    assert!(
        l.media(mid)
            .unwrap()
            .remote_pts()
            .contains(&EVENT_PT.into())
    );
    let audio_pt = l.params_opus().pt();
    let wallclock = l.last;
    let result = l
        .writer(mid)
        .unwrap()
        .telephone_event(tele(5, Duration::from_millis(100), 10))
        .write(
            audio_pt,
            wallclock,
            MediaTime::new(0, Frequency::FORTY_EIGHT_KHZ),
            [],
        );
    assert!(matches!(
        result,
        Err(RtcError::Packet(_, p, PacketError::InvalidTelephoneEvent(reason)))
            if p == audio_pt && reason == expected_reason
    ));
}

#[test]
fn telephone_event_write_selects_pt_from_audio_clock_rate() {
    init_crypto_default();

    let events = [
        (101, Frequency::EIGHT_KHZ, Some(7)),
        (102, Frequency::FORTY_EIGHT_KHZ, Some(7)),
    ];
    let codecs = [Codec::Opus, Codec::PCMU];
    let mut l = with_frame_audio_events(Peer::Left, &codecs, &events);
    let mut r = with_frame_audio_events(Peer::Right, &codecs, &events);
    let (mid, _, _) = negotiate_events(&mut l, &mut r, Direction::SendRecv);
    let pts = l.media(mid).unwrap().remote_pts();
    assert!(pts.contains(&101.into()) && pts.contains(&102.into()));
    let wallclock = l.last;

    for (audio_pt, clock_rate, event_pt) in [
        (l.params_opus().pt(), Frequency::FORTY_EIGHT_KHZ, 102),
        (0.into(), Frequency::EIGHT_KHZ, 101),
    ] {
        let result = l
            .writer(mid)
            .unwrap()
            .telephone_event(tele(8, Duration::from_millis(100), 10))
            .write(audio_pt, wallclock, MediaTime::new(0, clock_rate), []);
        assert!(matches!(
            result,
            Err(RtcError::Packet(_, p, PacketError::InvalidTelephoneEvent("event exceeds negotiated range")))
                if p == event_pt.into()
        ));
    }
}

#[test]
fn telephone_event_write_direct_api() -> Result<(), RtcError> {
    init_crypto_default();

    let mut rtc = with_frame_events(Peer::Left, &[(EVENT_PT, Frequency::FORTY_EIGHT_KHZ, None)]);
    let mid: Mid = "audio".into();
    let mut direct = rtc.direct_api();
    direct.declare_media(mid, MediaKind::Audio);
    direct.declare_stream_tx(1.into(), None, mid, None);
    assert!(rtc.media(mid).unwrap().remote_pts().is_empty());

    let wallclock = rtc.last;
    let start = MediaTime::new(0, Frequency::FORTY_EIGHT_KHZ);
    let audio_pt = rtc.params_opus().pt();
    rtc.writer(mid)
        .unwrap()
        .telephone_event(tele(5, Duration::from_millis(100), 10))
        .write(audio_pt, wallclock, start, [])
}

#[test]
fn telephone_event_write_rejects_invalid_events() {
    init_log();
    init_crypto_default();

    let (mut l, mut r, mid) = connected_frame_mode();
    let audio_pt = l.params_opus().pt();
    let wallclock = l.start + l.duration();
    let start = MediaTime::new(960, Frequency::FORTY_EIGHT_KHZ);
    let mut write = |pt: Pt, event, millis, volume| {
        let duration = Duration::from_millis(millis);
        l.writer(mid)
            .unwrap()
            .telephone_event(tele(event, duration, volume))
            .write(pt, wallclock, start, [])
    };

    let result = write(EVENT_PT.into(), 16, 500, 0);
    assert!(matches!(result, Err(RtcError::UnknownPt(pt)) if pt == EVENT_PT.into()));

    let err = write(audio_pt, 17, 500, 0).unwrap_err();
    assert!(matches!(
        err,
        RtcError::Packet(m, pt, PacketError::InvalidTelephoneEvent("event exceeds negotiated range"))
            if m == mid && pt == EVENT_PT.into()
    ));
    assert!(
        err.to_string()
            .ends_with("Invalid telephone event: event exceeds negotiated range")
    );

    let err = write(audio_pt, 16, 500, 64).unwrap_err();
    assert!(matches!(
        err,
        RtcError::Packet(m, pt, PacketError::InvalidTelephoneEvent("volume exceeds 63"))
            if m == mid && pt == EVENT_PT.into()
    ));
    assert!(
        err.to_string()
            .ends_with("Invalid telephone event: volume exceeds 63")
    );

    // libwebrtc allows 40 ms to 6 s.
    for millis in [39, 6001] {
        let err = write(audio_pt, 16, millis, 0).unwrap_err();
        assert!(matches!(
            err,
            RtcError::Packet(m, pt, PacketError::InvalidTelephoneEvent(
                "duration must be between 40 ms and 6 s"
            )) if m == mid && pt == EVENT_PT.into()
        ));
        assert!(
            err.to_string()
                .ends_with("Invalid telephone event: duration must be between 40 ms and 6 s")
        );
    }
    assert!(write(audio_pt, 16, 40, 0).is_ok());

    let mut incomplete = tele(16, Duration::from_millis(100), 10);
    incomplete.end = false;
    let err = l
        .writer(mid)
        .unwrap()
        .telephone_event(incomplete)
        .write(audio_pt, wallclock, start, [])
        .unwrap_err();
    assert!(matches!(
        err,
        RtcError::Packet(
            _,
            _,
            PacketError::InvalidTelephoneEvent("event must have end set")
        )
    ));

    // Without the send stream, new writes and queued packets fail.
    let ssrc = l.direct_api().stream_tx_by_mid(mid, None).unwrap().ssrc();
    assert!(l.direct_api().remove_stream_tx(ssrc));
    let duration = Duration::from_millis(100);
    let writer = l.writer(mid).unwrap();
    let result =
        writer
            .telephone_event(tele(16, duration, 0))
            .write(audio_pt, wallclock, start, []);
    assert!(matches!(result, Err(RtcError::NoSenderSource)));
    let result = progress_for(&mut l, &mut r, Duration::from_millis(100));
    assert!(matches!(result, Err(RtcError::NoSenderSource)));

    let (mut l, _r, mid) = connected_frame_mode();
    let wallclock = l.last;
    let rid = "r0".into();
    let writer = l.writer(mid).unwrap().rid(rid);
    let result =
        writer
            .telephone_event(tele(16, duration, 0))
            .write(audio_pt, wallclock, start, []);
    assert!(matches!(result, Err(RtcError::UnknownRid(_))));
    l.direct_api()
        .declare_stream_tx(1234.into(), None, mid, Some(rid));
    let writer = l.writer(mid).unwrap().rid(rid);
    let result =
        writer
            .telephone_event(tele(16, duration, 0))
            .write(audio_pt, wallclock, start, []);
    assert!(result.is_ok());
}

const EVENT_PT: u8 = 101;

fn tele(event: u8, duration: Duration, volume: u8) -> TelephoneEvent {
    TelephoneEvent {
        event,
        end: true,
        volume,
        duration,
    }
}

fn with_frame_events(peer: Peer, events: &[(u8, Frequency, Option<u8>)]) -> TestRtc {
    with_frame_audio_events(peer, &[Codec::Opus], events)
}

fn with_frame_audio_events(
    peer: Peer,
    codecs: &[Codec],
    events: &[(u8, Frequency, Option<u8>)],
) -> TestRtc {
    TestRtc::new_with_config(peer, |config| {
        let mut config = config.clear_codecs();
        enable_audio_codecs(config.codec_config(), codecs);
        add_events(config.codec_config(), events);
        config
    })
}

fn connected_frame_mode() -> (TestRtc, TestRtc, Mid) {
    let events = [(EVENT_PT, Frequency::FORTY_EIGHT_KHZ, None)];
    let mut l = with_frame_events(Peer::Left, &events);
    let mut r = with_frame_events(Peer::Right, &events);
    l.add_host_candidate((Ipv4Addr::new(1, 1, 1, 1), 1000).into());
    r.add_host_candidate((Ipv4Addr::new(2, 2, 2, 2), 2000).into());

    let (mid, _, _) = negotiate_events(&mut l, &mut r, Direction::SendRecv);
    while !l.is_connected() || !r.is_connected() {
        progress(&mut l, &mut r).expect("clean progress");
    }
    let max = l.last.max(r.last);
    l.last = max;
    r.last = max;

    (l, r, mid)
}

fn write_audio(rtc: &mut TestRtc, mid: Mid, pt: Pt, timestamp: u64) -> Result<(), RtcError> {
    let wallclock = rtc.start + rtc.duration();
    let time = MediaTime::new(timestamp, Frequency::FORTY_EIGHT_KHZ);
    rtc.writer(mid)
        .unwrap()
        .write(pt, wallclock, time, [0xf8, 0xff, 0xfe])
}

fn advance_with_empty_audio(
    l: &mut TestRtc,
    r: &mut TestRtc,
    mid: Mid,
    audio_pt: Pt,
    start: std::time::Instant,
    rtp_start: MediaTime,
    duration_ms: u64,
) -> Result<(), RtcError> {
    for millis in (10..=duration_ms).step_by(10) {
        let wallclock = start + Duration::from_millis(millis);
        while l.last < wallclock {
            progress(l, r)?;
        }
        let rtp_time = MediaTime::new(rtp_start.numer() + millis * 48, rtp_start.frequency());
        l.writer(mid)
            .unwrap()
            .write(audio_pt, wallclock, rtp_time, [])?;
        progress(l, r)?;
    }
    Ok(())
}

fn progress_for(l: &mut TestRtc, r: &mut TestRtc, duration: Duration) -> Result<(), RtcError> {
    let deadline = l.duration() + duration;
    while l.duration() < deadline {
        progress(l, r)?;
    }
    Ok(())
}

fn telephone_events(rtc: &TestRtc) -> Vec<&MediaData> {
    rtc.events
        .iter()
        .filter_map(|(_, event)| match event {
            Event::MediaData(data) if data.params.spec().codec.is_tele() => Some(data),
            _ => None,
        })
        .collect()
}
