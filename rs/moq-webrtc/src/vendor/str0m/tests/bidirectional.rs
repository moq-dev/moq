use std::net::Ipv4Addr;
use std::time::Duration;

use str0m::format::Codec;
use str0m::media::{Direction, MediaKind};
use str0m::{Event, RtcError};

mod common;
use common::{Peer, TestRtc, init_crypto_default, init_log, progress};

#[test]
pub fn bidirectional_same_m_line() -> Result<(), RtcError> {
    init_log();
    init_crypto_default();

    let mut l = TestRtc::new(Peer::Left);
    let mut r = TestRtc::new(Peer::Right);

    l.add_host_candidate((Ipv4Addr::new(1, 1, 1, 1), 1000).into());
    r.add_host_candidate((Ipv4Addr::new(2, 2, 2, 2), 2000).into());

    let mut change = l.sdp_api();
    let mid = change.add_media(MediaKind::Audio, Direction::SendRecv, None, None, None);
    let (offer, pending) = change.apply().unwrap();

    let answer = r.rtc.sdp_api().accept_offer(offer)?;
    l.rtc.sdp_api().accept_answer(pending, answer)?;

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
    let data_b = vec![2_u8; 80];

    loop {
        {
            let wallclock = l.start + l.duration();
            let time = l.duration().into();
            l.writer(mid)
                .unwrap()
                .write(pt, wallclock, time, data_a.clone())?;
        }

        progress(&mut l, &mut r)?;

        {
            let wallclock = r.start + r.duration();
            let time = l.duration().into();
            r.writer(mid)
                .unwrap()
                .write(pt, wallclock, time, data_b.clone())?;
        }

        progress(&mut l, &mut r)?;

        if l.duration() > Duration::from_secs(12) {
            break;
        }
    }

    let media_count_r = r
        .events
        .iter()
        .filter(|(_, e)| matches!(e, Event::MediaData(_)))
        .count();

    assert!(
        media_count_r > 170,
        "Not enough MediaData at R: {}",
        media_count_r
    );

    let media_count_l = l
        .events
        .iter()
        .filter(|(_, e)| matches!(e, Event::MediaData(_)))
        .count();

    assert!(
        media_count_l > 300,
        "Not enough MediaData at L: {}",
        media_count_l
    );

    Ok(())
}
