use std::net::Ipv4Addr;
use std::time::{Duration, Instant};

use str0m::media::{Direction, MediaKind};
use str0m::rtp::RawPacket;
use str0m::{Rtc, RtcError};
use tracing::info_span;

mod common;
use common::{TestRtc, init_crypto_default, init_log, negotiate, progress};

#[test]
pub fn no_pre_ice_feedback() -> Result<(), RtcError> {
    init_log();
    init_crypto_default();

    let now = Instant::now();
    let l_rtc = Rtc::builder().enable_raw_packets(true).build(now);
    let r_rtc = Rtc::builder().enable_raw_packets(true).build(now);

    let mut l = TestRtc::new_with_rtc(info_span!("L"), l_rtc);
    let mut r = TestRtc::new_with_rtc(info_span!("R"), r_rtc);

    l.add_host_candidate((Ipv4Addr::new(1, 1, 1, 1), 1000).into());
    r.add_host_candidate((Ipv4Addr::new(2, 2, 2, 2), 2000).into());

    negotiate(&mut l, &mut r, |change| {
        change.add_media(MediaKind::Video, Direction::SendOnly, None, None, None)
    });

    // Before ICE is established, introduce a large time delta that spans at least one sender/receiver report
    // interval
    let mut t = l.last;
    l.handle_input(str0m::Input::Timeout(t))?;
    t += Duration::from_secs(10);
    l.handle_input(str0m::Input::Timeout(t))?;

    loop {
        if l.is_connected() && r.is_connected() {
            break;
        }

        progress(&mut l, &mut r)?;
    }

    for (_, event) in l.events.iter() {
        if let Some(RawPacket::RtcpTx(tx)) = event.as_raw_packet() {
            panic!(
                "Sender should not have generated feedback before ICE, but it output {:?}",
                tx
            );
        }
    }

    for (_, event) in r.events.iter() {
        if let Some(RawPacket::RtcpRx(rx)) = event.as_raw_packet() {
            panic!(
                "Receiver should not have generated feedback before ICE, but it output {:?}",
                rx
            );
        }
    }

    Ok(())
}
