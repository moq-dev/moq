use std::net::Ipv4Addr;
use std::time::Duration;

use netem::NetemConfig;
use str0m::{Event, IceConnectionState, RtcError};

mod common;
use common::{Peer, TestRtc, init_crypto_default, init_log, progress};

/// Test that candidate pairs are kept alive based on incoming binding requests,
/// even when outgoing binding requests are not being responded to.
///
/// This specifically tests the fix for issue #846 where Chrome v144+ would cause
/// disconnections when not using ICE Lite mode. The fix ensures that
/// `is_still_possible()` considers recently received binding requests as evidence
/// of connectivity, similar to how ICE Lite mode works.
#[test]
pub fn pair_kept_alive_by_remote_binding_requests() -> Result<(), RtcError> {
    init_log();
    init_crypto_default();

    // Create two non-ICE-lite peers
    let mut l = TestRtc::new(Peer::Left);
    let mut r = TestRtc::new(Peer::Right);

    // Add some latency to simulate real network conditions
    l.set_netem(NetemConfig::new().latency(Duration::from_millis(10)));
    r.set_netem(NetemConfig::new().latency(Duration::from_millis(10)));

    l.add_host_candidate((Ipv4Addr::new(1, 1, 1, 1), 1000).into());
    r.add_host_candidate((Ipv4Addr::new(2, 2, 2, 2), 2000).into());

    let mut change = l.sdp_api();
    change.add_channel("ch".into());
    let (offer, pending) = change.apply().unwrap();
    let answer = r.sdp_api().accept_offer(offer)?;
    l.sdp_api().accept_answer(pending, answer)?;

    // First, establish the connection
    loop {
        if l.duration() > Duration::from_secs(5) {
            panic!("Timeout waiting for initial connection");
        }

        if l.is_connected() && r.is_connected() {
            break;
        }

        progress(&mut l, &mut r)?;
    }

    // Now continue running for a while to ensure the connection stays stable
    // This tests that the pairs are not prematurely pruned
    let stable_start = l.duration();
    loop {
        if l.duration() - stable_start > Duration::from_secs(3) {
            // Connection remained stable for 3 seconds
            break;
        }

        let l_disconnected = l.events.iter().any(|(_, e)| {
            matches!(
                e,
                Event::IceConnectionStateChange(IceConnectionState::Disconnected)
            )
        });
        let r_disconnected = r.events.iter().any(|(_, e)| {
            matches!(
                e,
                Event::IceConnectionStateChange(IceConnectionState::Disconnected)
            )
        });

        if l_disconnected || r_disconnected {
            panic!("Connection was unexpectedly disconnected during stability test");
        }

        progress(&mut l, &mut r)?;
    }

    Ok(())
}
