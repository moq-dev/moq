//! Minimum transmit rate with fixed media layers and unchanged desired bandwidth.

use std::time::Duration;

use netem::{DataSize, NetemConfig};
use str0m::RtcError;
use str0m::bwe::Bitrate;

use crate::common::{BweTestContext, connect_with_bwe, init_crypto_default, init_log};

fn recovery_time(minimum: Bitrate) -> Result<Duration, RtcError> {
    let desired = Bitrate::mbps(5);
    let (mut l, mut r) = connect_with_bwe(Bitrate::mbps(2), desired);
    let mut ctx = BweTestContext::new(&mut l, &mut r);

    let good = NetemConfig::new()
        .latency(Duration::from_millis(20))
        .link(Bitrate::mbps(5), DataSize::kbytes(100))
        .seed(42);
    l.set_netem(good);
    r.set_netem(good);
    ctx.run_for_duration(&mut l, &mut r, Duration::from_secs(5))?;

    r.set_netem(
        NetemConfig::new()
            .latency(Duration::from_millis(40))
            .link(Bitrate::mbps(1), DataSize::kbytes(50))
            .seed(42),
    );
    ctx.run_for_duration(&mut l, &mut r, Duration::from_secs(2))?;

    // The allocator drops to a fixed layer; desired bandwidth remains unchanged.
    ctx.set_media_send_rate(Bitrate::kbps(500));
    l.bwe().set_bitrate(minimum, desired);
    r.set_netem(good);
    // Measure time to regain a 2 Mbps layer, rather than comparing eventual
    // estimates after both senders have had time to run periodic probes.
    for half_seconds in 1..=20 {
        let estimate = ctx
            .run_for_duration(&mut l, &mut r, Duration::from_millis(500))?
            .expect("a bandwidth estimate after recovery");
        if estimate >= Bitrate::mbps(2) {
            return Ok(Duration::from_millis(half_seconds * 500));
        }
    }
    panic!("did not recover to 2 Mbps within 10 seconds with minimum {minimum}");
}

#[test]
fn minimum_improves_recovery_with_a_fixed_layer() -> Result<(), RtcError> {
    init_log();
    init_crypto_default();
    let automatic = recovery_time(Bitrate::ZERO)?;
    let sustained = recovery_time(Bitrate::mbps(2))?;
    assert!(
        sustained < Duration::from_secs(8),
        "recovery: {sustained:?}"
    );
    assert!(
        sustained < automatic,
        "sustained traffic took {sustained:?}, automatic padding took {automatic:?}"
    );
    Ok(())
}

#[test]
fn minimum_fills_media_gaps_and_can_be_lowered_without_probing() -> Result<(), RtcError> {
    use std::time::Instant;

    use str0m::{Event, Rtc};

    use crate::common::{TestRtc, connect_l_r_with_rtc};

    fn wire_rate(rtc: &TestRtc, since: usize) -> Bitrate {
        let mut stats = rtc.events[since..].iter().filter_map(|(_, event)| {
            if let Event::PeerStats(stats) = event {
                Some(stats)
            } else {
                None
            }
        });
        let first = stats.next().expect("first traffic sample");
        let last = stats.next_back().expect("last traffic sample");
        let bytes = DataSize::bytes((last.peer_bytes_tx - first.peer_bytes_tx) as i64);
        bytes / last.timestamp.duration_since(first.timestamp)
    }

    init_log();
    init_crypto_default();
    let start = Instant::now();
    let sender = Rtc::builder()
        .set_rtp_mode(true)
        .enable_bwe(Some(Bitrate::mbps(1)))
        .set_stats_interval(Some(Duration::from_millis(100)))
        .build(start);
    let receiver = Rtc::builder().set_rtp_mode(true).build(start);
    let (mut l, mut r) = connect_l_r_with_rtc(sender, receiver);
    l.set_forced_time_advance(Duration::from_micros(100));
    r.set_forced_time_advance(Duration::from_micros(100));
    let mut ctx = BweTestContext::new(&mut l, &mut r);
    ctx.set_media_send_rate(Bitrate::kbps(150));
    let desired = Bitrate::mbps(1);
    l.bwe().set_bitrate(Bitrate::ZERO, desired);
    ctx.run_for_duration(&mut l, &mut r, Duration::from_secs(2))?;

    let offset = l.events.len();
    l.bwe().set_bitrate(Bitrate::kbps(500), desired);
    ctx.run_for_duration(&mut l, &mut r, Duration::from_millis(800))?;
    let padded = wire_rate(&l, offset);
    assert!(padded > Bitrate::kbps(400), "transmit floor: {padded}");
    assert!(padded < Bitrate::kbps(650), "total traffic: {padded}");

    // Media replaces padding rather than adding to it.
    let media_offset = l.events.len();
    ctx.set_media_send_rate(Bitrate::kbps(600));
    ctx.run_for_duration(&mut l, &mut r, Duration::from_millis(800))?;
    let media = wire_rate(&l, media_offset);
    assert!(media > Bitrate::kbps(550), "media rate: {media}");
    assert!(
        media < Bitrate::kbps(800),
        "padding must not add 500 kbps: {media}"
    );

    let lower_offset = l.events.len();
    ctx.set_media_send_rate(Bitrate::kbps(150));
    l.bwe().set_bitrate(Bitrate::ZERO, desired);
    ctx.run_for_duration(&mut l, &mut r, Duration::from_millis(800))?;
    let lowered = wire_rate(&l, lower_offset);
    assert!(lowered < Bitrate::kbps(250), "lowered traffic: {lowered}");
    assert!(
        l.events[offset..]
            .iter()
            .all(|(_, e)| !matches!(e, Event::Probe(_))),
        "changing the minimum must not force a probe"
    );
    // An explicit minimum cannot sustain traffic after the sending media is removed.
    l.bwe().set_bitrate(Bitrate::kbps(500), desired);
    l.direct_api().remove_media("vid".into());
    ctx.set_media_send_rate(Bitrate::ZERO);
    let removed_offset = l.events.len();
    ctx.run_for_duration(&mut l, &mut r, Duration::from_millis(800))?;
    let removed = wire_rate(&l, removed_offset);
    assert!(
        removed < Bitrate::kbps(50),
        "traffic without sending media: {removed}"
    );
    Ok(())
}
