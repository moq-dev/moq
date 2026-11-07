use std::net::Ipv4Addr;
use std::time::{Duration, Instant};

use netem::NetemConfig;
use str0m::{Event, IceConnectionState, RtcConfig, RtcError};
use tracing::info_span;

mod common;
use common::{Peer, TestRtc, init_crypto_default, init_log, progress};

#[test]
pub fn ice_lite_premature_disconnect() -> Result<(), RtcError> {
    init_log();
    init_crypto_default();

    let mut l = TestRtc::new(Peer::Left);

    let rtc = RtcConfig::new().set_ice_lite(true).build(Instant::now());
    let mut r = TestRtc::new_with_rtc(info_span!("R"), rtc);

    r.set_netem(NetemConfig::new().latency(Duration::from_millis(5)));

    l.add_host_candidate((Ipv4Addr::new(1, 1, 1, 1), 1000).into());
    r.add_host_candidate((Ipv4Addr::new(2, 2, 2, 2), 2000).into());

    let mut change = l.sdp_api();
    change.add_channel("ch".into());
    let (offer, pending) = change.apply().unwrap();
    let answer = r.sdp_api().accept_offer(offer)?;
    l.sdp_api().accept_answer(pending, answer)?;

    loop {
        if l.duration() > Duration::from_secs(5) {
            panic!("Timeout waiting for connection");
        }

        let r_disconnected = r.events.iter().any(|(_, e)| {
            matches!(
                e,
                Event::IceConnectionStateChange(IceConnectionState::Disconnected)
            )
        });

        if r_disconnected {
            panic!("ICE-lite peer disconnected before receiving binding requests");
        }

        if l.is_connected() && r.is_connected() {
            break;
        }

        progress(&mut l, &mut r)?;
    }

    Ok(())
}
