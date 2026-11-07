use std::net::Ipv4Addr;
use std::time::{Duration, Instant};

use str0m::RtcConfig;
use str0m::RtcError;
use tracing::info_span;

mod common;
use common::{Peer, TestRtc, init_crypto_default, init_log, progress};

#[test]
pub fn ice_restart() -> Result<(), RtcError> {
    init_log();
    init_crypto_default();

    let mut l = TestRtc::new(Peer::Left);

    let rtc = RtcConfig::new().set_ice_lite(true).build(Instant::now());
    let mut r = TestRtc::new_with_rtc(info_span!("R"), rtc);

    l.add_host_candidate((Ipv4Addr::new(1, 1, 1, 1), 1000).into());
    r.add_host_candidate((Ipv4Addr::new(2, 2, 2, 2), 2000).into());

    let (offer, pending) = l.span.in_scope(|| {
        let mut change = l.rtc.sdp_api();
        let _ = change.add_channel("My little channel".into());

        change.apply().unwrap()
    });
    println!("L Initial Offer: {}", offer);

    let answer = r.span.in_scope(|| r.rtc.sdp_api().accept_offer(offer))?;
    println!("R Initial answer: {}", answer);

    l.span
        .in_scope(|| l.rtc.sdp_api().accept_answer(pending, answer))?;

    loop {
        if l.is_connected() && r.is_connected() {
            break;
        }
        progress(&mut l, &mut r)?;
    }

    let l_creds = l._local_ice_creds();
    let r_creds = r._local_ice_creds();

    let (offer, pending) = r.span.in_scope(|| {
        let mut change = r.rtc.sdp_api();
        change.ice_restart(true);

        change.apply().expect("Should be able to apply changes")
    });
    println!("R Offer: {}", offer);

    let answer = l.span.in_scope(|| l.rtc.sdp_api().accept_offer(offer))?;
    println!("L Answer: {}", answer);
    r.span
        .in_scope(|| r.rtc.sdp_api().accept_answer(pending, answer))?;

    assert!(!l.rtc.is_connected());
    assert!(!r.rtc.is_connected());

    loop {
        if l.duration() > Duration::from_secs(10) {
            panic!("Failed to re-establish connectivity after ICE restart in 10 seconds");
        }

        if l.is_connected() && r.is_connected() {
            break;
        }

        progress(&mut l, &mut r)?;
    }

    assert_ne!(
        r_creds,
        r._local_ice_creds(),
        "After an ICE restart ICE credentials should have changed"
    );
    assert_ne!(
        l_creds,
        l._local_ice_creds(),
        "After an ICE restart ICE credentials should have changed"
    );

    Ok(())
}
