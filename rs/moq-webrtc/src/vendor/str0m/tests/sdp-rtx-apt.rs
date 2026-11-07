use std::time::Instant;

use str0m::change::{SdpAnswer, SdpOffer};
use str0m::error::SdpError;
use str0m::format::Codec;
use str0m::media::{Direction, MediaKind};
use str0m::{Rtc, RtcError};

mod common;
use common::init_crypto_default;

fn offer_sdp(direction: &str, fmtp: &str) -> String {
    format!(
        "v=0\r\n\
         o=- 1 0 IN IP4 0.0.0.0\r\n\
         s=-\r\n\
         t=0 0\r\n\
         a=fingerprint:sha-256 AA:AA:AA:AA:AA:AA:AA:AA:AA:AA:AA:AA:AA:AA:AA:AA:\
         AA:AA:AA:AA:AA:AA:AA:AA:AA:AA:AA:AA:AA:AA:AA:AA\r\n\
         a=group:BUNDLE 0\r\n\
         m=video 9 UDP/TLS/RTP/SAVPF 120 121 125\r\n\
         c=IN IP4 0.0.0.0\r\n\
         a={direction}\r\n\
         a=mid:0\r\n\
         a=ice-ufrag:00000000\r\n\
         a=ice-pwd:00000000000000000000000000000000\r\n\
         a=setup:actpass\r\n\
         a=rtcp-mux\r\n\
         a=rtpmap:120 VP8/90000\r\n\
         a=rtpmap:121 VP9/90000\r\n\
         a=rtpmap:125 rtx/90000\r\n\
         {fmtp}"
    )
}

fn assert_conflicting_apt<T: std::fmt::Debug>(result: Result<T, RtcError>) {
    let Err(RtcError::Sdp(SdpError::ParseError(message) | SdpError::Inconsistent(message))) =
        result
    else {
        panic!("SDP with conflicting RTX apt values must be rejected: {result:?}");
    };
    assert!(message.contains("conflicting apt values"), "{message}");
}

#[test]
fn offer_with_conflicting_rtx_apt_is_rejected() {
    init_crypto_default();

    for direction in ["recvonly", "sendrecv", "sendonly", "inactive"] {
        for fmtp in [
            "a=fmtp:125 apt=120\r\na=fmtp:125 apt=121\r\n",
            "a=fmtp:125 apt=120;apt=121\r\n",
        ] {
            let mut rtc = Rtc::builder().build(Instant::now());
            let result = SdpOffer::from_sdp_string(&offer_sdp(direction, fmtp))
                .map_err(RtcError::from)
                .and_then(|offer| rtc.sdp_api().accept_offer(offer));

            assert_conflicting_apt(result);
            assert!(rtc.media("0".into()).is_none());
        }
    }
}

#[test]
fn answer_with_conflicting_rtx_apt_is_rejected() {
    init_crypto_default();

    for same_line in [false, true] {
        let mut local = Rtc::builder().build(Instant::now());
        let mut remote = Rtc::builder().build(Instant::now());
        let mut change = local.sdp_api();
        let mid = change.add_media(MediaKind::Video, Direction::SendRecv, None, None, None);
        let (offer, pending) = change.apply().unwrap();
        let answer = remote.sdp_api().accept_offer(offer).unwrap();

        let params = remote.codec_config();
        let vp8 = params
            .iter()
            .find(|p| p.spec().codec == Codec::Vp8)
            .unwrap();
        let vp9 = params
            .iter()
            .find(|p| p.spec().codec == Codec::Vp9)
            .unwrap();
        let rtx = vp8.resend().unwrap();
        let original = format!("a=fmtp:{rtx} apt={}", vp8.pt());
        let extra = if same_line {
            format!("{original};apt={}", vp9.pt())
        } else {
            format!("{original}\r\na=fmtp:{rtx} apt={}", vp9.pt())
        };
        let sdp = answer.to_sdp_string();
        assert!(sdp.contains(&original));
        let result = SdpAnswer::from_sdp_string(&sdp.replace(&original, &extra))
            .map_err(RtcError::from)
            .and_then(|answer| local.sdp_api().accept_answer(pending, answer));

        assert_conflicting_apt(result);
        assert!(local.media(mid).is_none());
    }
}

#[test]
fn repeated_identical_rtx_apt_is_accepted() {
    init_crypto_default();

    for fmtp in [
        "a=fmtp:125 apt=120\r\na=fmtp:125 apt=120\r\n",
        "a=fmtp:125 apt=120;apt=120\r\n",
    ] {
        let mut rtc = Rtc::builder().build(Instant::now());
        let offer = SdpOffer::from_sdp_string(&offer_sdp("recvonly", fmtp)).unwrap();
        let answer = rtc.sdp_api().accept_offer(offer).unwrap();

        assert!(answer.to_sdp_string().contains("a=fmtp:125 apt=120\r\n"));
        let vp8 = rtc
            .codec_config()
            .iter()
            .find(|p| p.spec().codec == Codec::Vp8)
            .unwrap();
        assert_eq!(vp8.resend(), Some(125.into()));
    }
}

#[test]
fn distinct_rtx_payload_types_are_accepted() {
    init_crypto_default();

    let mut rtc = Rtc::builder().build(Instant::now());
    let sdp = offer_sdp(
        "recvonly",
        "a=fmtp:125 apt=120\r\na=rtpmap:126 rtx/90000\r\na=fmtp:126 apt=121\r\n",
    )
    .replace("SAVPF 120 121 125", "SAVPF 120 121 125 126");
    let offer = SdpOffer::from_sdp_string(&sdp).unwrap();
    rtc.sdp_api().accept_offer(offer).unwrap();

    for (codec, rtx) in [(Codec::Vp8, 125), (Codec::Vp9, 126)] {
        let params = rtc
            .codec_config()
            .iter()
            .find(|p| p.spec().codec == codec)
            .unwrap();
        assert_eq!(params.resend(), Some(rtx.into()));
    }
}
