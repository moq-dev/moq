use super::*;

fn timed_fixture(unordered: bool) -> Association {
    let mut a = Association {
        state: AssociationState::Established,
        use_forward_tsn: true,
        my_next_tsn: 1,
        max_send_message_size: 100,
        max_receive_message_size: 100,
        max_payload_size: 4,
        cwnd: 100,
        rwnd: 100,
        mtu: 1400,
        ..Default::default()
    };
    a.create_stream(1, false, PayloadProtocolIdentifier::Binary)
        .unwrap();
    let mut stream = a.stream(1).unwrap();
    stream
        .set_reliability_params(unordered, ReliabilityType::Timed, 10)
        .unwrap();
    stream.write(b"abcdefghij").unwrap();
    a
}

#[test]
fn timed_recovery_obeys_exact_deadline() {
    let now = Instant::now();
    for fast in [false, true] {
        for unordered in [false, true] {
            for delay in [9, 10, 20, 2_000] {
                let mut a = timed_fixture(unordered);
                a.pop_pending_data_chunks_to_send(now);
                for tsn in 1..=3 {
                    let c = a.outbound_queue.get_mut(tsn).unwrap();
                    c.retransmit = true;
                    c.miss_indicator = 3;
                }
                let later = now + Duration::from_millis(delay);
                let packets = if fast {
                    a.will_retransmit_fast = true;
                    a.gather_outbound_fast_retransmission_packets(vec![], later)
                } else {
                    a.get_data_packets_to_retransmit(later)
                        .into_iter()
                        .map(|p| p.marshal().unwrap())
                        .collect()
                };
                assert_eq!(packets.is_empty(), delay >= 10);
                for tsn in 1..=3 {
                    assert_eq!(a.outbound_queue.is_abandoned(tsn), delay >= 10);
                }
            }
        }
    }
}

#[test]
fn timed_expiry_is_not_hidden_by_window_or_fast_packet_limit() {
    let now = Instant::now();
    for fast in [false, true] {
        let mut a = timed_fixture(false);
        a.pop_pending_data_chunks_to_send(now);
        // An earlier reliable message exhausts the available send budget.
        let first = a.outbound_queue.get_mut(1).unwrap();
        first.deadline = None;
        first.reliability = Some((ReliabilityType::Reliable, 0));
        first.message_id = u64::MAX;
        for tsn in 1..=3 {
            let c = a.outbound_queue.get_mut(tsn).unwrap();
            c.retransmit = true;
            c.miss_indicator = 3;
        }
        a.cwnd = 0;
        a.mtu = 0;
        let later = now + Duration::from_millis(10);
        if fast {
            a.will_retransmit_fast = true;
            assert!(
                a.gather_outbound_fast_retransmission_packets(vec![], later)
                    .is_empty()
            );
        } else {
            assert!(a.get_data_packets_to_retransmit(later).is_empty());
        }
        assert!(!a.outbound_queue.is_abandoned(1));
        assert!(a.outbound_queue.is_abandoned(2));
        assert!(a.outbound_queue.is_abandoned(3));
        assert_eq!(a.outbound_queue.get(1).unwrap().nsent, 1);
    }
}

#[test]
fn timed_expiry_abandons_pending_tail_at_tsn_wrap_without_retry() {
    let now = Instant::now();
    for unordered in [false, true] {
        let mut a = timed_fixture(unordered);
        a.my_next_tsn = u32::MAX;
        a.cumulative_tsn_ack_point = u32::MAX - 1;
        a.advanced_peer_tsn_ack_point = u32::MAX - 1;
        a.send_next_data_chunk(now).unwrap();
        // Expiry must run even when no retransmission has been requested and
        // before a pending sibling could be sent for the first time.
        let later = now + Duration::from_millis(10);
        assert!(a.pop_pending_data_chunks_to_send(later).is_empty());
        assert!(a.outbound_queue.is_abandoned(u32::MAX));
        assert!(a.outbound_queue.is_abandoned(0));
        assert_eq!(a.outbound_queue.pending_len(), 0);
        assert_eq!(a.stream(1).unwrap().buffered_amount().unwrap(), 0);
        assert_eq!(a.outbound_queue.get(u32::MAX).unwrap().nsent, 1);
    }
}

#[test]
fn timed_expiry_preserves_delivered_message_and_gap_acked_siblings() {
    let now = Instant::now();
    for all_delivered in [false, true] {
        let mut a = timed_fixture(false);
        a.pop_pending_data_chunks_to_send(now);
        let end = if all_delivered { 3 } else { 2 };
        for tsn in 1..=end {
            let released = a.outbound_queue.mark_as_acked(tsn);
            a.streams
                .get_mut(&1)
                .unwrap()
                .on_buffer_released(released as i64);
        }
        let later = now + Duration::from_secs(2);
        a.outbound_queue.mark_all_to_retransmit();
        assert!(a.get_data_packets_to_retransmit(later).is_empty());
        assert_eq!(a.outbound_queue.is_abandoned(3), !all_delivered);
        assert_eq!(a.stream(1).unwrap().buffered_amount().unwrap(), 0);
        // Repeated recovery must neither refund bytes again nor send DATA.
        a.outbound_queue.mark_all_to_retransmit();
        assert!(a.get_data_packets_to_retransmit(later).is_empty());
        assert_eq!(a.stream(1).unwrap().buffered_amount().unwrap(), 0);
        for tsn in 1..=end {
            assert!(a.outbound_queue.get(tsn).unwrap().acked);
            assert_eq!(a.outbound_queue.get(tsn).unwrap().nsent, 1);
        }
    }
}

#[test]
fn timed_expiry_preserves_reliable_dcep_and_non_negotiated_traffic() {
    let now = Instant::now();
    for control in 0..3 {
        let mut a = timed_fixture(false);
        a.pop_pending_data_chunks_to_send(now);
        if control == 2 {
            a.use_forward_tsn = false;
        }
        for tsn in 1..=3 {
            let c = a.outbound_queue.get_mut(tsn).unwrap();
            c.retransmit = true;
            match control {
                0 => {
                    c.reliability = Some((ReliabilityType::Reliable, 0));
                    c.deadline = None;
                }
                1 => c.payload_type = PayloadProtocolIdentifier::Dcep,
                _ => {}
            }
        }
        assert!(
            !a.get_data_packets_to_retransmit(now + Duration::from_secs(2))
                .is_empty()
        );
        assert!(!a.outbound_queue.is_abandoned(1));
    }
}

#[test]
fn timed_expiry_uses_original_message_deadline_after_stream_changes() {
    let now = Instant::now();
    let mut a = timed_fixture(false);
    a.send_next_data_chunk(now).unwrap();
    a.stream(1)
        .unwrap()
        .set_reliability_params(false, ReliabilityType::Reliable, 0)
        .unwrap();
    // A later fragment's transmission time cannot restart the lifetime.
    a.send_next_data_chunk(now + Duration::from_millis(9))
        .unwrap();
    assert!(
        a.pop_pending_data_chunks_to_send(now + Duration::from_millis(10))
            .is_empty()
    );
    assert!(a.outbound_queue.is_abandoned(1));
    assert!(a.outbound_queue.is_abandoned(2));
    assert!(a.outbound_queue.is_abandoned(3));
}

#[test]
fn timed_shutdown_retries_lost_forward_tsn() {
    let now = Instant::now();
    for state in [
        AssociationState::ShutdownPending,
        AssociationState::ShutdownSent,
        AssociationState::ShutdownReceived,
    ] {
        let mut a = timed_fixture(false);
        a.gather_outbound(now);
        a.state = state;
        let later = now + Duration::from_secs(2);
        let (packets, _) = a.gather_outbound(later);
        let contains_forward = |packets: &[Bytes]| {
            packets.iter().any(|raw| {
                Packet::unmarshal(raw)
                    .unwrap()
                    .chunks
                    .iter()
                    .any(|c| c.as_any().is::<ChunkForwardTsn>())
            })
        };
        assert!(contains_forward(&packets));
        assert!(a.timers.get(Timer::T3RTX).is_some());
        // Lose the first FORWARD-TSN, then recover through caller-controlled T3.
        a.on_retransmission_timeout(Timer::T3RTX, 1);
        let (packets, _) = a.gather_outbound(later + Duration::from_secs(3));
        assert!(contains_forward(&packets));
        assert!(a.timers.get(Timer::T3RTX).is_some());
        for raw in packets {
            assert!(
                Packet::unmarshal(&raw)
                    .unwrap()
                    .chunks
                    .iter()
                    .all(|c| !c.as_any().is::<ChunkPayloadData>())
            );
        }
    }
}

#[test]
fn timed_expiry_abandons_tail_after_entire_prefix_was_gap_acked() {
    let now = Instant::now();
    let mut a = timed_fixture(false);
    a.send_next_data_chunk(now).unwrap();
    let released = a.outbound_queue.mark_as_acked(1);
    a.streams
        .get_mut(&1)
        .unwrap()
        .on_buffer_released(released as i64);
    assert!(
        a.pop_pending_data_chunks_to_send(now + Duration::from_millis(10))
            .is_empty()
    );
    assert_eq!(a.outbound_queue.pending_len(), 0);
    assert!(a.outbound_queue.is_abandoned(2));
    assert!(a.outbound_queue.get(1).unwrap().acked);
    assert_eq!(a.stream(1).unwrap().buffered_amount().unwrap(), 0);
    assert!(!a.gather_outbound_forward_tsn_packets(vec![]).is_empty());
}

#[test]
fn timed_expiry_retains_forward_recovery_after_prefix_cumulative_ack() {
    let now = Instant::now();
    let mut a = timed_fixture(false);
    a.send_next_data_chunk(now).unwrap();
    a.process_selective_ack(
        &ChunkSelectiveAck {
            cumulative_tsn_ack: 1,
            ..Default::default()
        },
        now,
    )
    .unwrap();
    a.cumulative_tsn_ack_point = 1;
    a.streams.get_mut(&1).unwrap().on_buffer_released(4);
    assert_eq!(a.outbound_queue.inflight_len(), 0);
    assert!(a.timers.get(Timer::T3RTX).is_none());
    assert!(
        a.pop_pending_data_chunks_to_send(now + Duration::from_millis(10))
            .is_empty()
    );
    assert_eq!(a.outbound_queue.pending_len(), 0);
    assert_eq!(a.stream(1).unwrap().buffered_amount().unwrap(), 0);
    assert!(a.outbound_queue.is_abandoned(2));
    assert!(!a.gather_outbound_forward_tsn_packets(vec![]).is_empty());
    assert!(a.timers.get(Timer::T3RTX).is_some());
}

#[test]
fn pending_timed_messages_keep_distinct_lifetimes_across_polls() {
    let now = Instant::now();
    let mut a = timed_fixture(false);
    a.create_stream(2, false, PayloadProtocolIdentifier::Binary)
        .unwrap();
    let mut blocker = a.stream(2).unwrap();
    blocker
        .set_reliability_params(true, ReliabilityType::Reliable, 0)
        .unwrap();
    blocker.write(b"hold").unwrap();
    a.cwnd = 4;
    let chunks = a.pop_pending_data_chunks_to_send(now);
    assert_eq!(chunks.len(), 1);
    assert_eq!(chunks[0].stream_identifier, 2);

    a.stream(1).unwrap().write(b"new").unwrap();
    assert!(
        a.pop_pending_data_chunks_to_send(now + Duration::from_millis(5))
            .is_empty()
    );
    assert_eq!(a.stream(1).unwrap().buffered_amount().unwrap(), 13);
    assert!(
        a.pop_pending_data_chunks_to_send(now + Duration::from_millis(10))
            .is_empty()
    );
    assert_eq!(a.stream(1).unwrap().buffered_amount().unwrap(), 3);
    assert_eq!(a.my_next_tsn, 2, "the unsent expired message needs no TSN");

    a.cwnd = 100;
    let chunks = a.pop_pending_data_chunks_to_send(now + Duration::from_millis(14));
    assert_eq!(chunks.len(), 1);
    assert_eq!(chunks[0].user_data.as_ref(), b"new");
    assert_eq!(chunks[0].stream_sequence_number, 0);
    assert_eq!(chunks[0].deadline, Some(now + Duration::from_millis(15)));
    assert!(
        a.get_data_packets_to_retransmit(now + Duration::from_millis(15))
            .is_empty()
    );
    assert!(a.outbound_queue.is_abandoned(2));
    assert_eq!(a.stream(1).unwrap().buffered_amount().unwrap(), 0);
}
