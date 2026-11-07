//! Tests that media is correctly delivered under packet loss conditions.
//!
//! Uses the netem crate to simulate realistic bursty packet loss and verifies
//! that RTP retransmissions recover all lost packets.

mod common;

use std::time::Duration;

use common::{connect_l_r, init_crypto_default, progress, vp8_data};
use netem::{GilbertElliot, LossModel, NetemConfig};
use str0m::format::Codec;
use str0m::media::MediaKind;
use str0m::rtp::{RtpWrite, Ssrc};
use str0m::{Event, RtcError};

use crate::common::init_log;

/// Run a media transmission test with the given loss model using real VP8 data.
///
/// Returns the number of packets received.
fn run_loss_test(loss_model: impl Into<LossModel>, seed: u64) -> Result<usize, RtcError> {
    init_log();
    init_crypto_default();

    let (mut l, mut r) = connect_l_r();

    // Configure netem on R's incoming queue (L -> R has loss)
    let config = NetemConfig::new().loss(loss_model).seed(seed);
    r.set_netem(config);

    let mid = "vid".into();
    let ssrc_tx: Ssrc = 42.into();
    let ssrc_rtx: Ssrc = 44.into();

    l.direct_api().declare_media(mid, MediaKind::Video);
    l.direct_api()
        .declare_stream_tx(ssrc_tx, Some(ssrc_rtx), mid, None);

    // Increase the RTX ratio cap to 0.2 to allow more retransmissions.
    // With heavy loss, the RTX ratio naturally grows, and the default
    // cap (0.15) is too low to allow all needed retransmissions.
    l.direct_api().stream_tx(&ssrc_tx).unwrap().set_rtx_cache(
        1024,
        Duration::from_secs(3),
        Some(0.2),
    );

    r.direct_api().declare_media(mid, MediaKind::Video);
    r.direct_api()
        .expect_stream_rx(ssrc_tx, Some(ssrc_rtx), mid, None);

    let max = l.last.max(r.last);
    l.last = max;
    r.last = max;

    let params = l.params_vp8();
    assert_eq!(params.spec().codec, Codec::Vp8);
    let pt = params.pt();

    let data = vp8_data();
    let packet_count = data.len();

    for (relative, header, payload) in data {
        // Keep RTC time progressed to be "in sync" with the test data
        while (l.last - max) < relative {
            progress(&mut l, &mut r)?;
        }

        let absolute = max + relative;

        let mut direct = l.direct_api();
        let tx = direct.stream_tx(&ssrc_tx).unwrap();
        tx.write_rtp(
            RtpWrite::new(
                pt,
                header.sequence_number(None),
                header.timestamp,
                absolute,
                payload,
            )
            .marker(header.marker)
            .nackable(true),
        );

        progress(&mut l, &mut r)?;

        if l.duration() > Duration::from_secs(10) {
            break;
        }
    }

    // Let retransmissions complete (also subject to loss)
    let settle_time = l.duration() + Duration::from_secs(2);
    while l.duration() < settle_time {
        progress(&mut l, &mut r)?;
    }

    // Count received RTP packets
    let mut received_seqs: Vec<u64> = r
        .events
        .iter()
        .filter_map(|(_, e)| {
            if let Event::RtpPacket(v) = e {
                Some(*v.seq_no)
            } else {
                None
            }
        })
        .collect();

    received_seqs.sort();

    println!(
        "Sent {} packets, received {} packets",
        packet_count,
        received_seqs.len()
    );

    // Find which sequence numbers are missing
    if !received_seqs.is_empty() {
        let first = received_seqs[0];
        let last = *received_seqs.last().unwrap();
        let expected: Vec<u64> = (first..=last).collect();
        let missing: Vec<u64> = expected
            .iter()
            .filter(|s| !received_seqs.contains(s))
            .copied()
            .collect();
        if !missing.is_empty() {
            println!("Missing seq numbers from Event::RtpPacket: {:?}", missing);
        }
    }

    Ok(received_seqs.len())
}

#[test]
fn loss_light() -> Result<(), RtcError> {
    // ~1% loss - should recover everything easily
    let loss = GilbertElliot::wifi();
    let received = run_loss_test(loss, 12345)?;

    // VP8 data has 104 packets, with light loss we should get all
    assert_eq!(
        received, 104,
        "Expected 104 packets with light loss, got {}",
        received
    );
    Ok(())
}

#[test]
fn loss_medium() -> Result<(), RtcError> {
    // ~5% loss
    let loss = GilbertElliot::wifi_lossy();
    let received = run_loss_test(loss, 12345)?;

    assert_eq!(
        received, 104,
        "Expected 104 packets with medium loss, got {}",
        received
    );
    Ok(())
}

#[test]
fn loss_heavy() -> Result<(), RtcError> {
    // ~10% loss
    let loss = GilbertElliot::congested();
    let received = run_loss_test(loss, 12345)?;

    assert_eq!(
        received, 104,
        "Expected 104 packets with heavy loss, got {}",
        received
    );
    Ok(())
}
