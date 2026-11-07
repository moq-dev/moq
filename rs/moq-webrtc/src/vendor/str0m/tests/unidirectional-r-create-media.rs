use std::net::Ipv4Addr;
use std::time::Duration;

use str0m::change::SdpOffer;
use str0m::format::Codec;
use str0m::media::{Direction, MediaKind};
use str0m::{Event, RtcError};

mod common;
use common::{Peer, TestRtc, init_crypto_default, init_log, progress};

#[test]
pub fn unidirectional_r_create_media() -> Result<(), RtcError> {
    init_log();
    init_crypto_default();

    let mut l = TestRtc::new(Peer::Left);
    let mut r = TestRtc::new(Peer::Right);

    l.add_host_candidate((Ipv4Addr::new(1, 1, 1, 1), 1000).into());
    r.add_host_candidate((Ipv4Addr::new(2, 2, 2, 2), 2000).into());

    // The change is on the R (not sending side) with Direction::RecvOnly.
    let mut change = r.sdp_api();
    let mid = change.add_media(MediaKind::Audio, Direction::RecvOnly, None, None, None);
    let (offer, pending) = change.apply().unwrap();

    // str0m always produces a=ssrc lines, also for RecvOnly (since direction can change).
    // We munge the a=ssrc lines away.
    let mut offer = offer.to_string();
    let start = offer.find("a=ssrc").unwrap();
    offer.replace_range(start..offer.len(), "");

    let offer = SdpOffer::from_sdp_string(&offer).unwrap();
    let answer = l.rtc.sdp_api().accept_offer(offer)?;
    r.rtc.sdp_api().accept_answer(pending, answer)?;

    loop {
        if l.is_connected() || r.is_connected() {
            break;
        }
        progress(&mut l, &mut r)?;
    }

    let max = l.last.max(r.last);
    l.last = max;
    r.last = max;

    let params = l.params_opus();
    assert_eq!(params.spec().codec, Codec::Opus);
    let pt = params.pt();

    let data_a = vec![1_u8; 80];

    loop {
        let wallclock = l.start + l.duration();
        let time = l.duration().into();
        l.writer(mid)
            .unwrap()
            .write(pt, wallclock, time, data_a.clone())?;

        progress(&mut l, &mut r)?;
        progress(&mut l, &mut r)?;

        if l.duration() > Duration::from_secs(10) {
            break;
        }
    }

    let media_count = r
        .events
        .iter()
        .filter(|(_, e)| matches!(e, Event::MediaData(_)))
        .count();

    assert!(media_count > 80, "Not enough MediaData: {}", media_count);

    Ok(())
}
