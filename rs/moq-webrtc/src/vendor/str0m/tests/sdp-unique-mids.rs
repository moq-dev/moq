use str0m::RtcError;
use str0m::change::{SdpAnswer, SdpApi, SdpOffer};
use str0m::error::SdpError;
use str0m::media::{Direction, MediaKind, Mid};

mod common;
use common::{Peer, TestRtc, init_crypto_default, init_log, negotiate};

fn repeat_next_mid() -> Mid {
    fastrand::seed(1);
    let mid = Mid::new();
    fastrand::seed(1);
    mid
}

fn offer(l: &mut TestRtc, r: &mut TestRtc, do_change: impl FnOnce(&mut SdpApi)) -> Vec<String> {
    let mut change = l.rtc.sdp_api();
    do_change(&mut change);
    let (offer, pending) = change.apply().unwrap();

    let mids = offer
        .to_sdp_string()
        .lines()
        .filter_map(|l| l.strip_prefix("a=mid:"))
        .map(|m| m.trim().to_string())
        .collect();

    let answer = r.rtc.sdp_api().accept_offer(offer).unwrap();
    l.rtc.sdp_api().accept_answer(pending, answer).unwrap();

    mids
}

fn add_audio(change: &mut SdpApi) -> Mid {
    change.add_media(MediaKind::Audio, Direction::SendRecv, None, None, None)
}

#[test]
pub fn media_and_channel_in_one_offer_get_distinct_mids() {
    init_log();
    init_crypto_default();

    let mut l = TestRtc::new(Peer::Left);
    let mut r = TestRtc::new(Peer::Right);

    let mids = offer(&mut l, &mut r, |change| {
        let expected = repeat_next_mid();
        assert_eq!(add_audio(change), expected);
        repeat_next_mid();
        change.add_channel("data".into());
    });

    assert_eq!(mids.len(), 2);
    assert_ne!(mids[0], mids[1]);
}

#[test]
pub fn two_media_in_one_offer_get_distinct_mids() {
    init_log();
    init_crypto_default();

    let mut l = TestRtc::new(Peer::Left);
    let mut r = TestRtc::new(Peer::Right);

    let mut added = vec![];
    let mids = offer(&mut l, &mut r, |change| {
        let expected = repeat_next_mid();
        added.push(add_audio(change));
        assert_eq!(added[0], expected);
        repeat_next_mid();
        added.push(add_audio(change));
    });

    assert_ne!(added[0], added[1]);
    assert_eq!(mids, [added[0].to_string(), added[1].to_string()]);
    assert!(r.rtc.media(added[1]).is_some());
}

#[test]
pub fn media_offered_after_channel_avoids_its_mid() {
    init_log();
    init_crypto_default();

    let mut l = TestRtc::new(Peer::Left);
    let mut r = TestRtc::new(Peer::Right);

    let channel_mid = repeat_next_mid();
    negotiate(&mut l, &mut r, |change| {
        change.add_channel("data".into());
    });

    let mut audio = None;
    let mids = offer(&mut l, &mut r, |change| {
        repeat_next_mid();
        audio = Some(add_audio(change));
    });

    let audio = audio.unwrap();
    assert_ne!(audio, channel_mid);
    assert_eq!(mids, [channel_mid.to_string(), audio.to_string()]);
    assert!(r.rtc.media(audio).is_some());
}

fn share_first_mid(sdp: &str) -> (String, String) {
    let mids: Vec<&str> = sdp
        .lines()
        .filter_map(|l| l.strip_prefix("a=mid:"))
        .map(str::trim)
        .collect();
    assert_eq!(mids.len(), 2);
    let shared = sdp
        .replace(&format!("a=mid:{}", mids[1]), &format!("a=mid:{}", mids[0]))
        .replace(
            &format!("a=group:BUNDLE {} {}", mids[0], mids[1]),
            &format!("a=group:BUNDLE {} {}", mids[0], mids[0]),
        );
    (shared, mids[0].to_string())
}

fn assert_inconsistent_mid<T: std::fmt::Debug>(result: Result<T, RtcError>, mid: &str) {
    let Err(RtcError::Sdp(SdpError::Inconsistent(message))) = result else {
        panic!("SDP with a duplicate mid must be rejected: {result:?}");
    };
    assert_eq!(
        message,
        format!("Mid {mid} is used by more than one m-line")
    );
}

#[test]
pub fn offer_with_duplicate_mids_is_rejected() {
    init_log();
    init_crypto_default();

    let mut l = TestRtc::new(Peer::Left);
    let mut r = TestRtc::new(Peer::Right);

    let mut change = l.rtc.sdp_api();
    add_audio(&mut change);
    change.add_channel("data".into());
    let (offer, _pending) = change.apply().unwrap();

    let (sdp, mid) = share_first_mid(&offer.to_sdp_string());
    let offer = SdpOffer::from_sdp_string(&sdp).unwrap();

    assert_inconsistent_mid(r.rtc.sdp_api().accept_offer(offer), &mid);
    assert!(r.rtc.media(mid.as_str().into()).is_none());
}

#[test]
pub fn answer_with_duplicate_mids_is_rejected() {
    init_log();
    init_crypto_default();

    let mut l = TestRtc::new(Peer::Left);
    let mut r = TestRtc::new(Peer::Right);

    let mut change = l.rtc.sdp_api();
    let audio = add_audio(&mut change);
    change.add_channel("data".into());
    let (offer, pending) = change.apply().unwrap();
    let answer = r.rtc.sdp_api().accept_offer(offer).unwrap();

    let (sdp, mid) = share_first_mid(&answer.to_sdp_string());
    assert_eq!(mid, audio.to_string());
    let answer = SdpAnswer::from_sdp_string(&sdp).unwrap();

    assert_inconsistent_mid(l.rtc.sdp_api().accept_answer(pending, answer), &mid);
    assert!(l.rtc.media(audio).is_none());
}
