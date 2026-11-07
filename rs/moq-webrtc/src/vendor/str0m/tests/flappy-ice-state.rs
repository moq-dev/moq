use std::net::Ipv4Addr;
use std::time::{Duration, Instant};

use str0m::RtcConfig;
use str0m::{Event, RtcError};
use tracing::info_span;

mod common;
use common::{Peer, TestRtc, init_crypto_default, init_log, progress};

#[test]
pub fn flappy_ice_lite_state() -> Result<(), RtcError> {
    init_log();
    init_crypto_default();

    let mut l = TestRtc::new(Peer::Left);

    let rtc = RtcConfig::new().set_ice_lite(true).build(Instant::now());
    let mut r = TestRtc::new_with_rtc(info_span!("R"), rtc);

    l.add_host_candidate((Ipv4Addr::new(1, 1, 1, 1), 1000).into());
    r.add_host_candidate((Ipv4Addr::new(2, 2, 2, 2), 2000).into());

    let mut change = l.sdp_api();
    let _ = change.add_channel("My little channel".into());
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

    loop {
        progress(&mut l, &mut r)?;

        if l.duration() > Duration::from_secs(120) {
            break;
        }
    }

    let ice_events = r
        .events
        .iter()
        .filter(|(_, e)| matches!(e, Event::IceConnectionStateChange(_)))
        .count();

    assert!(ice_events < 10);

    Ok(())
}
