use std::net::Ipv4Addr;
use std::time::{Duration, Instant};

use str0m::format::Codec;
use str0m::media::{Direction, MediaKind};
use str0m::rtp::{RtpWrite, Ssrc};
use str0m::stats::{MediaEgressStats, PeerStats};
use str0m::{Event, RtcConfig, RtcError};
use tracing::info_span;

mod common;
use common::{TestRtc, connect_l_r_with_rtc, init_crypto_default, init_log, progress};

#[test]
pub fn stats() -> Result<(), RtcError> {
    init_log();
    init_crypto_default();

    let l_config = RtcConfig::new().set_stats_interval(Some(Duration::from_secs(10)));
    let r_config = RtcConfig::new().set_stats_interval(Some(Duration::from_secs(10)));

    let now = Instant::now();
    let mut l = TestRtc::new_with_rtc(info_span!("L"), l_config.build(now));
    let mut r = TestRtc::new_with_rtc(info_span!("R"), r_config.build(now));

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

    l.set_forced_time_advance(Duration::from_millis(1));
    r.set_forced_time_advance(Duration::from_millis(1));

    loop {
        {
            let wallclock = l.start + l.duration();
            let time = l.duration().into();
            l.writer(mid)
                .unwrap()
                .write(pt, wallclock, time, data_a.clone())?;
        }

        {
            let wallclock = r.start + r.duration();
            let time = l.duration().into();
            r.writer(mid)
                .unwrap()
                .write(pt, wallclock, time, data_b.clone())?;
        }

        progress(&mut l, &mut r)?;

        if l.duration() > Duration::from_secs(25) {
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

    let egress_stats_l: Vec<MediaEgressStats> = l
        .events
        .iter()
        .filter(|(_, e)| matches!(e, Event::MediaEgressStats(_)))
        .map(|(_, e)| {
            if let Event::MediaEgressStats(stats) = e {
                stats.clone()
            } else {
                panic!("Unexpected event type!")
            }
        })
        .collect();

    egress_stats_l
        .iter()
        .filter_map(|egress_stat_l| egress_stat_l.rtt)
        // rtt should be under 100ms in this scenario
        .for_each(|rtt| assert!(rtt < Duration::from_millis(100)));

    let egress_stats_r: Vec<MediaEgressStats> = l
        .events
        .iter()
        .filter(|(_, e)| matches!(e, Event::MediaEgressStats(_)))
        .map(|(_, e)| {
            if let Event::MediaEgressStats(stats) = e {
                stats.clone()
            } else {
                panic!("Unexpected event type!")
            }
        })
        .collect();

    egress_stats_r
        .iter()
        .filter_map(|egress_stat_l| egress_stat_l.rtt)
        // rtt should be under 100ms in this scenario
        .for_each(|rtt| assert!(rtt < Duration::from_millis(100)));
    assert!(
        media_count_l > 1100,
        "Not enough MediaData at L: {}",
        media_count_l
    );

    Ok(())
}

#[test]
pub fn peer_media_stats_do_not_drop_when_streams_are_removed() -> Result<(), RtcError> {
    init_log();
    init_crypto_default();

    let now = Instant::now();
    let config_l = RtcConfig::new()
        .set_rtp_mode(true)
        .set_stats_interval(Some(Duration::from_secs(1)));
    let config_r = RtcConfig::new()
        .set_rtp_mode(true)
        .set_stats_interval(Some(Duration::from_secs(1)));
    let (mut l, mut r) = connect_l_r_with_rtc(config_l.build(now), config_r.build(now));

    let mid = "aud".into();
    let ssrc: Ssrc = 1.into();

    l.direct_api().declare_media(mid, MediaKind::Audio);
    l.direct_api().declare_stream_tx(ssrc, None, mid, None);
    r.direct_api().declare_media(mid, MediaKind::Audio);
    r.direct_api().expect_stream_rx(ssrc, None, mid, None);

    let max = l.last.max(r.last);
    l.last = max;
    r.last = max;

    let params = l.params_opus();
    assert_eq!(params.spec().codec, Codec::Opus);
    let pt = params.pt();

    for i in 0..3 {
        let wallclock = l.start + l.duration();
        let time = (48_000 * i) as u32;
        let seq_no = (1000 + i).into();
        let payload = vec![i as u8; 80];

        l.direct_api()
            .stream_tx(&ssrc)
            .unwrap()
            .write_rtp(RtpWrite::new(pt, seq_no, time, wallclock, payload));

        progress(&mut l, &mut r)?;
    }

    let before_l = wait_for_peer_stats(&mut l, &mut r, true, |s| s.bytes_tx > 0)?;
    let before_r = wait_for_peer_stats(&mut l, &mut r, false, |s| s.bytes_rx > 0)?;

    l.direct_api().remove_media(mid);
    r.direct_api().remove_media(mid);

    let after_l = wait_for_peer_stats(&mut l, &mut r, true, |s| s.timestamp > before_l.timestamp)?;
    let after_r = wait_for_peer_stats(&mut l, &mut r, false, |s| s.timestamp > before_r.timestamp)?;

    assert_eq!(after_l.bytes_tx, before_l.bytes_tx);
    assert_eq!(after_r.bytes_rx, before_r.bytes_rx);

    Ok(())
}

fn wait_for_peer_stats(
    l: &mut TestRtc,
    r: &mut TestRtc,
    left: bool,
    predicate: impl Fn(&PeerStats) -> bool,
) -> Result<PeerStats, RtcError> {
    for _ in 0..1000 {
        progress(l, r)?;

        let events = if left { &l.events } else { &r.events };
        if let Some(stats) = events.iter().rev().find_map(|(_, e)| {
            if let Event::PeerStats(stats) = e {
                predicate(stats).then(|| stats.clone())
            } else {
                None
            }
        }) {
            return Ok(stats);
        }
    }

    panic!("timed out waiting for PeerStats");
}
