use std::net::Ipv4Addr;
use std::time::{Duration, Instant};

use str0m::channel::ChannelConfig;
use str0m::{Candidate, Event, RtcConfig, RtcError};
use tracing::info_span;

mod common;
use common::{Peer, TestRtc, init_crypto_default, init_log, progress};

#[test]
pub fn data_channel_direct() -> Result<(), RtcError> {
    init_log();
    init_crypto_default();

    let mut l = TestRtc::new(Peer::Left);

    let rtc_r = RtcConfig::new().set_ice_lite(true).build(Instant::now());
    let mut r = TestRtc::new_with_rtc(info_span!("R"), rtc_r);

    let host1 = Candidate::host((Ipv4Addr::new(1, 1, 1, 1), 1000).into(), "udp")?;
    let host2 = Candidate::host((Ipv4Addr::new(2, 2, 2, 2), 2000).into(), "udp")?;
    l.add_local_candidate(host1.clone()).unwrap();
    l.add_remote_candidate(host2.clone());
    r.add_local_candidate(host2).unwrap();
    r.add_remote_candidate(host1);

    let finger_l = l.direct_api().local_dtls_fingerprint().clone();
    let finger_r = r.direct_api().local_dtls_fingerprint().clone();

    l.direct_api().set_remote_fingerprint(finger_r);
    r.direct_api().set_remote_fingerprint(finger_l);

    let creds_l = l.direct_api().local_ice_credentials();
    let creds_r = r.direct_api().local_ice_credentials();

    l.direct_api().set_remote_ice_credentials(creds_r);
    r.direct_api().set_remote_ice_credentials(creds_l);

    l.direct_api().set_ice_controlling(true);
    r.direct_api().set_ice_controlling(false);

    l.direct_api().start_dtls(true).unwrap();
    r.direct_api().start_dtls(false).unwrap();

    l.direct_api().start_sctp(true);
    r.direct_api().start_sctp(false);

    let config = ChannelConfig {
        negotiated: Some(1),
        label: "my-chan".into(),
        ..Default::default()
    };
    let cid = l.direct_api().create_data_channel(config.clone());
    r.direct_api().create_data_channel(config);

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
        if let Some(mut chan) = l.channel(cid) {
            chan.write(false, "Hello world! ".as_bytes())
                .expect("to write string");
        }

        progress(&mut l, &mut r)?;

        if l.duration() > Duration::from_secs(10) {
            break;
        }
    }

    l.direct_api().close_data_channel(cid);

    loop {
        progress(&mut l, &mut r)?;

        if l.duration() > Duration::from_secs(12) {
            break;
        }
    }

    assert!(
        l.events
            .iter()
            .any(|(_, event)| event == &Event::ChannelOpen(cid, "my-chan".into()))
    );
    assert!(r.events.len() > 120);
    assert!(
        l.events
            .iter()
            .any(|(_, event)| event == &Event::ChannelClose(cid))
    );

    // Assert that ChannelOpen happens quickly after IceConnectionStateChange(Completed)
    let ice_completed_time = l
        .events
        .iter()
        .find_map(|(t, e)| match e {
            Event::IceConnectionStateChange(str0m::IceConnectionState::Completed) => Some(*t),
            _ => None,
        })
        .expect("IceConnectionStateChange(Completed) event");
    let channel_open_time = l
        .events
        .iter()
        .find_map(|(t, e)| match e {
            Event::ChannelOpen(_, _) => Some(*t),
            _ => None,
        })
        .expect("ChannelOpen event");
    let channel_open_delay = channel_open_time.duration_since(ice_completed_time);
    assert!(
        channel_open_delay < Duration::from_millis(250),
        "ChannelOpen should happen within 250ms of ICE completing, but took {:?}",
        channel_open_delay
    );

    Ok(())
}
