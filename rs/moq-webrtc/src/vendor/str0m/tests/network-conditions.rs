//! Tests for realistic network simulation conditions.

use std::net::Ipv4Addr;
use std::time::Duration;

use netem::{NetemConfig, Probability, RandomLoss};
use str0m::format::Codec;
use str0m::media::{Direction, MediaKind};
use str0m::{Event, RtcError};

mod common;
use common::{Peer, TestRtc, init_crypto_default, init_log, negotiate, progress};

/// Test connection with high latency (500ms+).
#[test]
fn network_high_latency_500ms() -> Result<(), RtcError> {
    init_log();
    init_crypto_default();

    let mut l = TestRtc::new(Peer::Left);
    let mut r = TestRtc::new(Peer::Right);

    l.add_host_candidate((Ipv4Addr::new(1, 1, 1, 1), 1000).into());
    r.add_host_candidate((Ipv4Addr::new(2, 2, 2, 2), 2000).into());

    // Apply high latency to both directions
    l.set_netem(NetemConfig::new().latency(Duration::from_millis(500)));
    r.set_netem(NetemConfig::new().latency(Duration::from_millis(500)));

    let mid = negotiate(&mut l, &mut r, |change| {
        change.add_media(MediaKind::Audio, Direction::SendRecv, None, None, None)
    });

    // Connection might take longer with high latency
    loop {
        if l.is_connected() && r.is_connected() {
            break;
        }
        if l.duration() > Duration::from_secs(30) {
            panic!("Failed to connect with high latency");
        }
        progress(&mut l, &mut r)?;
    }

    let max = l.last.max(r.last);
    l.last = max;
    r.last = max;

    // Send and receive audio
    let params = l.params_opus();
    assert_eq!(params.spec().codec, Codec::Opus);
    let pt = params.pt();
    let data = vec![1_u8; 80];

    let mut received_count = 0;
    let send_until = l.duration() + Duration::from_secs(5);

    loop {
        if l.duration() >= send_until {
            break;
        }

        let wallclock = l.start + l.duration();
        let time = l.duration().into();
        l.writer(mid)
            .unwrap()
            .write(pt, wallclock, time, data.clone())?;
        progress(&mut l, &mut r)?;

        received_count = r
            .events
            .iter()
            .filter(|(_, e)| matches!(e, Event::MediaData(_)))
            .count();
    }

    // Should still receive data despite high latency
    assert!(
        received_count > 10,
        "Should receive data with high latency, got {}",
        received_count
    );

    Ok(())
}

/// Test connection with packet loss.
#[test]
fn network_packet_loss() -> Result<(), RtcError> {
    init_log();
    init_crypto_default();

    let mut l = TestRtc::new(Peer::Left);
    let mut r = TestRtc::new(Peer::Right);

    l.add_host_candidate((Ipv4Addr::new(1, 1, 1, 1), 1000).into());
    r.add_host_candidate((Ipv4Addr::new(2, 2, 2, 2), 2000).into());

    let mid = negotiate(&mut l, &mut r, |change| {
        change.add_media(MediaKind::Audio, Direction::SendRecv, None, None, None)
    });

    loop {
        if l.is_connected() && r.is_connected() {
            break;
        }
        if l.duration() > Duration::from_secs(10) {
            panic!("Failed to connect");
        }
        progress(&mut l, &mut r)?;
    }

    let max = l.last.max(r.last);
    l.last = max;
    r.last = max;

    // Apply 10% packet loss to receiver
    r.set_netem(
        NetemConfig::new()
            .loss(RandomLoss::new(Probability::new(0.1)))
            .seed(42),
    );

    let params = l.params_opus();
    let pt = params.pt();
    let data = vec![1_u8; 80];

    let send_until = l.duration() + Duration::from_secs(5);

    loop {
        if l.duration() >= send_until {
            break;
        }

        let wallclock = l.start + l.duration();
        let time = l.duration().into();
        l.writer(mid)
            .unwrap()
            .write(pt, wallclock, time, data.clone())?;
        progress(&mut l, &mut r)?;
    }

    let received_count = r
        .events
        .iter()
        .filter(|(_, e)| matches!(e, Event::MediaData(_)))
        .count();

    // Should still receive most data (90% with 10% loss)
    assert!(
        received_count > 50,
        "Should receive most data with 10% loss, got {}",
        received_count
    );

    Ok(())
}

/// Test recovery after network outage.
#[test]
fn network_recovery_from_outage() -> Result<(), RtcError> {
    init_log();
    init_crypto_default();

    let mut l = TestRtc::new(Peer::Left);
    let mut r = TestRtc::new(Peer::Right);

    l.add_host_candidate((Ipv4Addr::new(1, 1, 1, 1), 1000).into());
    r.add_host_candidate((Ipv4Addr::new(2, 2, 2, 2), 2000).into());

    let mid = negotiate(&mut l, &mut r, |change| {
        change.add_media(MediaKind::Audio, Direction::SendRecv, None, None, None)
    });

    loop {
        if l.is_connected() && r.is_connected() {
            break;
        }
        progress(&mut l, &mut r)?;
    }

    let max = l.last.max(r.last);
    l.last = max;
    r.last = max;

    let params = l.params_opus();
    let pt = params.pt();
    let data = vec![1_u8; 80];

    // Phase 1: Normal operation
    let phase1_until = l.duration() + Duration::from_millis(500);
    loop {
        if l.duration() >= phase1_until {
            break;
        }
        let wallclock = l.start + l.duration();
        let time = l.duration().into();
        l.writer(mid)
            .unwrap()
            .write(pt, wallclock, time, data.clone())?;
        progress(&mut l, &mut r)?;
    }

    // Phase 2: Simulated outage (100% loss)
    r.set_netem(NetemConfig::new().loss(RandomLoss::new(Probability::ONE)));
    let phase2_until = l.duration() + Duration::from_millis(500);
    loop {
        if l.duration() >= phase2_until {
            break;
        }
        let wallclock = l.start + l.duration();
        let time = l.duration().into();
        l.writer(mid)
            .unwrap()
            .write(pt, wallclock, time, data.clone())?;
        progress(&mut l, &mut r)?;
    }

    // Phase 3: Recovery (restore network)
    r.set_netem(NetemConfig::new());
    let phase3_until = l.duration() + Duration::from_secs(2);
    loop {
        if l.duration() >= phase3_until {
            break;
        }
        let wallclock = l.start + l.duration();
        let time = l.duration().into();
        l.writer(mid)
            .unwrap()
            .write(pt, wallclock, time, data.clone())?;
        progress(&mut l, &mut r)?;
    }

    // Should have received data in phases 1 and 3
    let received_count = r
        .events
        .iter()
        .filter(|(_, e)| matches!(e, Event::MediaData(_)))
        .count();

    assert!(
        received_count > 20,
        "Should recover and receive data after outage, got {}",
        received_count
    );

    Ok(())
}

/// Test asymmetric latency (different in each direction).
#[test]
fn network_asymmetric_latency() -> Result<(), RtcError> {
    init_log();
    init_crypto_default();

    let mut l = TestRtc::new(Peer::Left);
    let mut r = TestRtc::new(Peer::Right);

    l.add_host_candidate((Ipv4Addr::new(1, 1, 1, 1), 1000).into());
    r.add_host_candidate((Ipv4Addr::new(2, 2, 2, 2), 2000).into());

    // Different latency in each direction
    l.set_netem(NetemConfig::new().latency(Duration::from_millis(50))); // L -> R: 50ms
    r.set_netem(NetemConfig::new().latency(Duration::from_millis(200))); // R -> L: 200ms

    let mid = negotiate(&mut l, &mut r, |change| {
        change.add_media(MediaKind::Audio, Direction::SendRecv, None, None, None)
    });

    loop {
        if l.is_connected() && r.is_connected() {
            break;
        }
        if l.duration() > Duration::from_secs(15) {
            panic!("Failed to connect with asymmetric latency");
        }
        progress(&mut l, &mut r)?;
    }

    let max = l.last.max(r.last);
    l.last = max;
    r.last = max;

    let params = l.params_opus();
    let pt = params.pt();
    let data = vec![1_u8; 80];

    let send_until = l.duration() + Duration::from_secs(3);

    loop {
        if l.duration() >= send_until {
            break;
        }

        let wallclock = l.start + l.duration();
        let time = l.duration().into();
        l.writer(mid)
            .unwrap()
            .write(pt, wallclock, time, data.clone())?;
        progress(&mut l, &mut r)?;
    }

    let received_count = r
        .events
        .iter()
        .filter(|(_, e)| matches!(e, Event::MediaData(_)))
        .count();

    assert!(
        received_count > 10,
        "Should work with asymmetric latency, got {}",
        received_count
    );

    Ok(())
}

/// Test moderate latency with some loss.
#[test]
fn network_latency_with_loss() -> Result<(), RtcError> {
    init_log();
    init_crypto_default();

    let mut l = TestRtc::new(Peer::Left);
    let mut r = TestRtc::new(Peer::Right);

    l.add_host_candidate((Ipv4Addr::new(1, 1, 1, 1), 1000).into());
    r.add_host_candidate((Ipv4Addr::new(2, 2, 2, 2), 2000).into());

    // Moderate latency with some loss
    l.set_netem(
        NetemConfig::new()
            .latency(Duration::from_millis(100))
            .loss(RandomLoss::new(Probability::new(0.02)))
            .seed(42),
    );
    r.set_netem(
        NetemConfig::new()
            .latency(Duration::from_millis(100))
            .loss(RandomLoss::new(Probability::new(0.02)))
            .seed(43),
    );

    let mid = negotiate(&mut l, &mut r, |change| {
        change.add_media(MediaKind::Audio, Direction::SendRecv, None, None, None)
    });

    loop {
        if l.is_connected() && r.is_connected() {
            break;
        }
        if l.duration() > Duration::from_secs(15) {
            panic!("Failed to connect");
        }
        progress(&mut l, &mut r)?;
    }

    let max = l.last.max(r.last);
    l.last = max;
    r.last = max;

    let params = l.params_opus();
    let pt = params.pt();
    let data = vec![1_u8; 80];

    let send_until = l.duration() + Duration::from_secs(5);

    loop {
        if l.duration() >= send_until {
            break;
        }

        let wallclock = l.start + l.duration();
        let time = l.duration().into();
        l.writer(mid)
            .unwrap()
            .write(pt, wallclock, time, data.clone())?;
        progress(&mut l, &mut r)?;
    }

    let received_count = r
        .events
        .iter()
        .filter(|(_, e)| matches!(e, Event::MediaData(_)))
        .count();

    // Should receive most data (98%+ with 2% loss)
    assert!(
        received_count > 80,
        "Should receive most data with latency and loss, got {}",
        received_count
    );

    Ok(())
}
