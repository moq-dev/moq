use std::net::Ipv4Addr;

use str0m::media::{Direction, MediaKind};
use str0m::rtp::Ssrc;
use str0m::{Event, RtcError};

mod common;
use common::{Peer, TestRtc, connect_l_r, init_crypto_default, init_log, progress};

#[test]
pub fn direct_declare_media_no_media_added_event() -> Result<(), RtcError> {
    init_log();
    init_crypto_default();

    let (mut l, mut r) = connect_l_r();

    let mid = "aud".into();

    // In this example we are using MID only (no RID) to identify the incoming media.
    let ssrc_tx: Ssrc = 42.into();

    l.direct_api().declare_media(mid, MediaKind::Audio);
    l.direct_api().declare_stream_tx(ssrc_tx, None, mid, None);

    r.direct_api().declare_media(mid, MediaKind::Audio);

    let max = l.last.max(r.last);
    l.last = max;
    r.last = max;

    loop {
        if l.is_connected() || r.is_connected() {
            break;
        }
        progress(&mut l, &mut r)?;
    }

    let found_local = l
        .events
        .iter()
        .any(|(_, e)| matches!(e, Event::MediaAdded(_)));

    let found_remote = r
        .events
        .iter()
        .any(|(_, e)| matches!(e, Event::MediaAdded(_)));

    assert!(!found_local, "declare_media with local MediaAdded");
    assert!(!found_remote, "declare_media found remote MediaAdded");

    Ok(())
}

#[test]
pub fn sdp_no_media_added_event() -> Result<(), RtcError> {
    init_log();
    init_crypto_default();

    let mut l = TestRtc::new(Peer::Left);
    let mut r = TestRtc::new(Peer::Right);

    l.add_host_candidate((Ipv4Addr::new(1, 1, 1, 1), 1000).into());
    r.add_host_candidate((Ipv4Addr::new(2, 2, 2, 2), 2000).into());

    let mut change = l.sdp_api();
    let _ = change.add_media(MediaKind::Audio, Direction::SendRecv, None, None, None);
    let (offer, pending) = change.apply().unwrap();

    let answer = r.rtc.sdp_api().accept_offer(offer)?;
    l.rtc.sdp_api().accept_answer(pending, answer)?;

    loop {
        if l.is_connected() || r.is_connected() {
            break;
        }
        progress(&mut l, &mut r)?;
    }

    let found_local = l
        .events
        .iter()
        .any(|(_, e)| matches!(e, Event::MediaAdded(_)));

    let found_remote = r
        .events
        .iter()
        .any(|(_, e)| matches!(e, Event::MediaAdded(_)));

    assert!(!found_local, "declare_media with local MediaAdded");
    assert!(found_remote, "declare_media found remote MediaAdded");

    Ok(())
}
