//! Compare complete sampling/control state against unchanged queue bookkeeping.

use super::*;

fn same(indexed: &Bbr3, scanned: &Bbr3) {
    let control = |bbr: &Bbr3| {
        let mut state = bbr.clone();
        state.packets = Packets::default();
        format!("{state:?}")
    };
    assert_eq!(
        control(indexed),
        control(scanned),
        "sampling or congestion state changed"
    );
    assert_eq!(
        format!("{:?}", indexed.packets.snapshot()),
        format!("{:?}", scanned.packets.snapshot())
    );
}

#[test]
fn indexed_cleanup_preserves_callback_samples_and_control() {
    let base = Instant::now();
    let rtt = RttEstimator::new(Duration::from_millis(10));
    let config = Bbr3Config {
        probe_rng_seed: Some([3; 16]),
        ..Default::default()
    };
    let mut indexed = Bbr3::new(Arc::new(config), 1200);
    let mut scanned = indexed.clone();
    scanned.packets = Packets::scanned();
    let mut initial_held = true;
    let mut handshake_held = true;
    for space in [SpaceId::Initial, SpaceId::Handshake] {
        for bbr in [&mut indexed, &mut scanned] {
            bbr.on_packet_sent(base, 1200, 0, space);
        }
    }
    same(&indexed, &scanned);
    for round in 0..40u64 {
        let sent = base + Duration::from_millis(round * 100);
        // Sparse packet numbers make any allocation or scan over numeric gaps visible.
        let first = round * 1_000_000;
        let count = if round % 2 == 0 { 2 } else { 1 };
        for pn in first..first + count {
            for bbr in [&mut indexed, &mut scanned] {
                bbr.on_packet_sent(sent, 1200, pn, SpaceId::Data);
            }
            same(&indexed, &scanned);
        }
        let now = sent + Duration::from_millis(20);
        if round == 2 {
            for bbr in [&mut indexed, &mut scanned] {
                bbr.on_packet_lost(1200, 0, SpaceId::Handshake, now);
            }
            handshake_held = false;
            same(&indexed, &scanned);
            for bbr in [&mut indexed, &mut scanned] {
                bbr.on_spurious_congestion_event();
            }
            same(&indexed, &scanned);
            // discard_space removes Initial from transport inflight without a
            // controller callback; its snapshot must age out through Data ACKs.
            initial_held = false;
        }
        // Alternate ordered/reordered and single/batched ACKs.
        let mut acked: Vec<_> = (first..first + count).collect();
        if round % 4 == 0 {
            acked.reverse();
        }
        let held = u64::from(initial_held) + u64::from(handshake_held);
        for (index, pn) in acked.into_iter().enumerate() {
            for bbr in [&mut indexed, &mut scanned] {
                bbr.on_ack(now, sent, 1200, pn, SpaceId::Data, round % 5 == 0, &rtt);
            }
            same(&indexed, &scanned);
            if count == 1 || round % 3 == 0 {
                let outstanding = held + count - index as u64 - 1;
                for bbr in [&mut indexed, &mut scanned] {
                    bbr.on_end_acks(now, outstanding * 1200, false, Some(pn), SpaceId::Data);
                }
                same(&indexed, &scanned);
            }
        }
        if count > 1 && round % 3 != 0 {
            for bbr in [&mut indexed, &mut scanned] {
                bbr.on_end_acks(
                    now,
                    held * 1200,
                    false,
                    Some(first + count - 1),
                    SpaceId::Data,
                );
            }
            same(&indexed, &scanned);
        }
        if round == 20 {
            // A late event after expiry must not borrow Data packet 0's metadata.
            for bbr in [&mut indexed, &mut scanned] {
                bbr.on_ack(now, base, 1200, 0, SpaceId::Initial, false, &rtt);
                bbr.on_end_acks(now, held * 1200, false, Some(0), SpaceId::Initial);
            }
            same(&indexed, &scanned);
        }
    }
}
