//! Pacer queue-time accounting with a send queue that never empties.

use std::time::Duration;

use str0m::bwe::Bitrate;
use str0m::media::MediaKind;
use str0m::rtp::{RtpWrite, Ssrc};
use str0m::{Event, RtcError};

use crate::common::{connect_with_bwe, init_crypto_default, init_log, progress};

#[test]
fn backlog_is_paced_without_bursts_or_gaps() -> Result<(), RtcError> {
    init_log();
    init_crypto_default();

    // Without TWCC feedback the estimate stays at its initial value, so pacing is ~1.1 Mbps.
    let (mut l, mut r) = connect_with_bwe(Bitrate::mbps(1), Bitrate::mbps(1));
    let mid = "vid".into();
    let ssrc: Ssrc = 42.into();
    let rtx: Ssrc = 44.into();
    l.direct_api().declare_media(mid, MediaKind::Video);
    l.direct_api().declare_stream_tx(ssrc, Some(rtx), mid, None);
    r.direct_api().declare_media(mid, MediaKind::Video);
    r.direct_api().expect_stream_rx(ssrc, Some(rtx), mid, None);
    let pt = l.params_vp8().pt();

    let media_bps = Bitrate::kbps(1_200).as_u64() as f64;
    let packet_size = 1150;
    let start = l.duration();
    let end = start + Duration::from_secs(12);
    let mut written = 0_u64;
    while l.duration() < end {
        let now = l.duration();
        let due = (now - start).as_secs_f64() * media_bps / 8.0 / packet_size as f64;
        while (written as f64) < due {
            let rtp = RtpWrite::new(pt, written.into(), 0, l.start + now, vec![0; packet_size]);
            l.direct_api().stream_tx(&ssrc).unwrap().write_rtp(rtp);
            written += 1;
        }
        progress(&mut l, &mut r)?;
    }

    // Without network emulation a packet arrives when it is sent. The harness clock can step
    // back, so sort before measuring.
    let mut arrivals: Vec<Duration> = r
        .events
        .iter()
        .filter(|(_, e)| matches!(e, Event::RtpPacket(p) if p.header.ssrc == ssrc))
        .map(|(t, _)| *t - r.start)
        .collect();
    arrivals.sort();
    let longest_gap = arrivals.windows(2).map(|w| w[1] - w[0]).max().unwrap();
    let tightest_six = arrivals.windows(6).map(|w| w[5] - w[0]).min().unwrap();
    // At ~1.1 Mbps a 1150 byte packet leaves every ~8 ms.
    assert!(
        longest_gap < Duration::from_millis(100),
        "no packet sent for {longest_gap:?}"
    );
    assert!(
        tightest_six >= Duration::from_millis(1),
        "6 packets sent within {tightest_six:?}"
    );
    Ok(())
}
