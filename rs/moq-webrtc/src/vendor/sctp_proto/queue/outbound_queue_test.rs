use super::*;
use bytes::Bytes;

fn message(stream: u16, unordered: bool, fragments: usize) -> Vec<ChunkPayloadData> {
    (0..fragments)
        .map(|i| ChunkPayloadData {
            stream_identifier: stream,
            unordered,
            beginning_fragment: i == 0,
            ending_fragment: i + 1 == fragments,
            user_data: Bytes::from_static(b"fragment"),
            ..Default::default()
        })
        .collect()
}

#[test]
fn unordered_priority_and_message_selection_survive_prefix_ack() {
    let now = Instant::now();
    let mut q = OutboundQueue::default();
    q.push(message(1, false, 3));
    q.push(message(2, false, 1));
    q.push(message(3, true, 1));
    q.push(message(4, true, 1));
    assert_eq!(q.pending_len(), 6);
    assert_eq!(q.pending_bytes(), 48);

    for (tsn, stream) in [(10, 3), (11, 4), (12, 1)] {
        assert_eq!(q.peek_pending().unwrap().stream_identifier, stream);
        assert_eq!(
            q.send_next(&mut { tsn }, now).unwrap().stream_identifier,
            stream
        );
        q.pop(tsn).unwrap();
    }
    // The entire sent prefix is gone, but the selected message must survive.
    assert_eq!(q.inflight_len(), 0);
    assert_eq!(q.inflight_bytes(), 0);
    assert!(!q.peek_pending().unwrap().beginning_fragment);
    assert!(q.pending_contains_stream(1));
    assert!(q.pending_contains_stream(2));
    assert!(!q.pending_contains_stream(3));
    q.push(message(5, true, 1));
    for (tsn, stream) in [(13, 1), (14, 1), (15, 5), (16, 2)] {
        let c = q.send_next(&mut { tsn }, now).unwrap();
        assert_eq!(c.stream_identifier, stream);
        assert_eq!(q.get(tsn).unwrap().tsn, tsn);
        assert!(q.get(tsn.wrapping_sub(1)).is_none());
        q.pop(tsn).unwrap();
    }
    assert!(q.peek_pending().is_none());
    assert!(q.send_next(&mut { 17 }, now).is_none());
    assert_eq!(q.pending_bytes(), 0);
    assert_eq!(q.pending_len(), 0);
    assert_eq!(q.inflight_bytes(), 0);
    assert_eq!(q.inflight_len(), 0);
}

#[test]
fn abandonment_discards_pending_tail_and_does_not_alias_unordered_messages() {
    let now = Instant::now();
    let mut q = OutboundQueue::default();
    // Both messages deliberately have the same stream and SSN.
    q.push(message(7, true, 5));
    q.push(message(7, true, 1));
    for tsn in 1..=3 {
        q.send_next(&mut { tsn }, now).unwrap();
    }
    q.pop(1).unwrap();
    assert_eq!(q.mark_as_acked(3), 8);
    assert_eq!(q.mark_as_acked(3), 0);
    assert_eq!(q.inflight_bytes(), 8);
    let mut next_tsn = 4;
    assert_eq!(q.abandon(2, &mut next_tsn), Some((7, 24)));
    assert_eq!(next_tsn, 5);
    assert!(q.is_abandoned(2));
    assert!(q.is_abandoned(3));
    assert!(!q.is_abandoned(1));
    assert_eq!(q.pending_len(), 1);
    assert_eq!(q.pending_bytes(), 8);
    assert_eq!(
        q.abandon(3, &mut next_tsn),
        Some((7, 0)),
        "release each byte only once"
    );

    // One terminal TSN skips the unsent tail without retaining its fragments.
    assert!(q.is_abandoned(4));
    assert!(q.get(4).unwrap().user_data.is_empty());
    assert_eq!(q.get(4).unwrap().nsent, 0);
    assert!(!q.get(4).unwrap().retransmit);
    assert!(q.get(5).is_none());
    let next = q.send_next(&mut { next_tsn }, now).unwrap();
    assert!(next.beginning_fragment && next.ending_fragment);
    assert!(!q.is_abandoned(5));
    assert!(!q.pending_contains_stream(7));
    for tsn in 2..=5 {
        q.pop(tsn).unwrap();
    }
    assert_eq!(q.inflight_len(), 0);
    assert_eq!(q.inflight_bytes(), 0);
}

#[test]
fn retransmission_excludes_pending_acked_and_abandoned_fragments() {
    let now = Instant::now();
    let mut q = OutboundQueue::default();
    q.push(message(1, false, 2));
    q.push(message(2, false, 3));
    for tsn in 1..=4 {
        q.send_next(&mut { tsn }, now).unwrap();
    }
    q.mark_all_to_retransmit();
    assert_eq!(q.mark_as_acked(2), 8);
    assert_eq!(q.abandon(1, &mut 5), Some((1, 8)));
    q.mark_all_to_retransmit();
    for tsn in 1..=4 {
        assert_eq!(q.get(tsn).unwrap().retransmit, tsn >= 3);
    }
    assert!(!q.peek_pending().unwrap().retransmit);
    assert!(q.get(5).is_none(), "pending chunks have no TSN yet");
    assert_eq!(q.inflight_bytes(), 16);
    assert_eq!(q.pending_bytes(), 8);
    q.get_mut(3).unwrap().nsent += 1;
    assert_eq!(q.get(3).unwrap().nsent, 2);
    assert_eq!(q.get(4).unwrap().nsent, 1);
}

#[test]
fn tsn_lookup_and_retirement_across_wraparound() {
    let now = Instant::now();
    let mut q = OutboundQueue::default();
    let first = u32::MAX - 4;
    for stream in 0..4 {
        q.push(message(stream, false, 3));
    }
    for i in 0..12 {
        q.send_next(&mut { first.wrapping_add(i) }, now).unwrap();
    }
    assert!(q.get(first.wrapping_sub(1)).is_none());
    assert!(q.get(first.wrapping_add(12)).is_none());
    assert!(q.pop(first.wrapping_add(1)).is_none());
    assert_eq!(q.abandon(u32::MAX, &mut 7), Some((1, 24)));
    for i in 0..12 {
        let tsn = first.wrapping_add(i);
        for remaining in i..12 {
            let next = first.wrapping_add(remaining);
            assert_eq!(
                q.get(next).unwrap().stream_identifier,
                (remaining / 3) as u16
            );
            assert_eq!(q.is_abandoned(next), (3..6).contains(&remaining));
        }
        assert_eq!(q.pop(tsn).unwrap().tsn, tsn);
        assert!(q.get(tsn).is_none());
        assert_eq!(q.inflight_len(), (11 - i) as usize);
        assert_eq!(
            q.inflight_bytes(),
            ((i + 1)..12).filter(|n| !(3..6).contains(n)).count() * 8
        );
    }
    assert_eq!(q.inflight_len(), 0);
}

#[test]
fn tsn_lookup_after_repeated_insertions_and_removals() {
    let now = Instant::now();
    let mut q = OutboundQueue::default();
    // Keep several messages in flight while repeatedly retiring and adding
    // messages, checking lookup after repeated insertions and removals.
    for tsn in 0..128u32 {
        q.push(message(tsn as u16, false, 1));
        q.send_next(&mut { tsn }, now).unwrap();
        if tsn >= 8 {
            q.pop(tsn - 8).unwrap();
        }
        for outstanding in tsn.saturating_sub(7)..=tsn {
            assert_eq!(
                q.get(outstanding).unwrap().stream_identifier,
                outstanding as u16
            );
        }
    }
    for tsn in 120..128 {
        q.pop(tsn).unwrap();
    }
    assert_eq!(q.inflight_len(), 0);
}

#[test]
fn reserving_abandoned_tail_tsns_crosses_wraparound() {
    let now = Instant::now();
    let mut q = OutboundQueue::default();
    q.push(message(1, false, 4));
    q.push(message(2, false, 1));
    q.send_next(&mut { u32::MAX - 1 }, now).unwrap();
    let mut next_tsn = u32::MAX;
    assert_eq!(q.abandon(u32::MAX - 1, &mut next_tsn), Some((1, 32)));
    assert_eq!(next_tsn, 0);
    assert_eq!(q.inflight_len(), 2);
    assert_eq!(q.inflight_bytes(), 0);
    q.mark_all_to_retransmit();
    for tsn in [u32::MAX - 1, u32::MAX] {
        assert!(q.is_abandoned(tsn));
        assert!(!q.get(tsn).unwrap().retransmit);
    }
    q.send_next(&mut next_tsn, now).unwrap();
    assert_eq!(q.get(0).unwrap().stream_identifier, 2);
    assert!(!q.is_abandoned(0));
    for tsn in [u32::MAX - 1, u32::MAX, 0] {
        q.pop(tsn).unwrap();
    }
    assert_eq!(q.inflight_bytes(), 0);
    assert_eq!(q.inflight_len(), 0);
}

#[test]
fn empty_message_does_not_block_scheduling() {
    let now = Instant::now();
    let mut q = OutboundQueue::default();
    q.push(message(1, true, 0));
    assert!(q.peek_pending().is_none());
    assert!(q.send_next(&mut { 1 }, now).is_none());
    assert!(q.get(1).is_none());
    assert_eq!(q.abandon(1, &mut 1), None);
    assert_eq!(q.mark_as_acked(1), 0);
    q.push(message(2, false, 1));
    assert_eq!(q.send_next(&mut { 1 }, now).unwrap().stream_identifier, 2);
    assert_eq!(q.pop(1).unwrap().stream_identifier, 2);
    assert_eq!(q.pending_len(), 0);
    assert_eq!(q.inflight_len(), 0);
}

#[test]
fn abandonment_matches_interleaved_fragments_by_message_tag() {
    let mut q = OutboundQueue::default();
    // Exercise the storage independently of the current non-interleaving DATA
    // scheduler. Stream ID, SSN, and unordered flag deliberately match.
    let first = u32::MAX - 2;
    for (i, (mut a, mut b)) in message(7, true, 3)
        .into_iter()
        .zip(message(7, true, 3))
        .enumerate()
    {
        a.message_id = 10;
        b.message_id = 11;
        a.tsn = first.wrapping_add(2 * i as u32);
        b.tsn = a.tsn.wrapping_add(1);
        q.inflight.push_no_check(a);
        q.inflight.push_no_check(b);
    }
    q.mark_all_to_retransmit();
    let mut next_tsn = first.wrapping_add(6);
    assert_eq!(
        q.abandon(first.wrapping_add(2), &mut next_tsn),
        Some((7, 24))
    );
    assert_eq!(next_tsn, first.wrapping_add(6));
    for i in 0..6 {
        let tsn = first.wrapping_add(i);
        assert_eq!(q.is_abandoned(tsn), i % 2 == 0);
        assert_eq!(q.get(tsn).unwrap().retransmit, i % 2 != 0);
        assert_eq!(q.get(tsn).unwrap().message_id, 10 + u64::from(i % 2));
    }
    for i in 0..6 {
        q.pop(first.wrapping_add(i)).unwrap();
    }
    assert_eq!(q.inflight_len(), 0);
    assert_eq!(q.inflight_bytes(), 0);
}

#[test]
fn removing_other_message_tail_preserves_pending_selection() {
    let now = Instant::now();
    let mut q = OutboundQueue::default();
    q.push(message(1, false, 3));
    q.send_next(&mut 1, now).unwrap();
    // Model an independently started message whose remaining fragments are
    // waiting behind the selected message in the flat pending queues.
    let mut other = message(2, true, 3).into_iter();
    let mut first = other.next().unwrap();
    first.message_id = 1;
    first.tsn = 2;
    q.inflight.push_no_check(first);
    for mut chunk in other {
        chunk.message_id = 1;
        q.pending.push(chunk);
    }
    q.next_message_id = 2;
    q.send_next(&mut 3, now).unwrap();
    let mut next_tsn = 4;
    assert_eq!(q.abandon(2, &mut next_tsn), Some((2, 24)));
    assert_eq!(next_tsn, 5);
    assert_eq!(q.pending_len(), 1);
    let remaining = q.send_next(&mut next_tsn, now).unwrap();
    assert_eq!(remaining.stream_identifier, 1);
    assert!(!remaining.beginning_fragment);
    assert!(remaining.ending_fragment);
    assert!(!remaining.abandoned);
}

#[test]
fn pending_expiry_deadlines_ssn_wrap_and_exemptions() {
    use core::time::Duration;
    let now = Instant::now();
    for unordered in [false, true] {
        for delay in [
            Duration::ZERO,
            Duration::from_millis(10),
            Duration::from_secs(u32::MAX as u64),
        ] {
            let mut q = OutboundQueue::default();
            let mut expired = message(1, unordered, 3);
            for c in &mut expired {
                c.deadline = Some(now + delay);
                c.stream_sequence_number = u16::MAX;
            }
            q.push(expired);
            q.push(message(1, unordered, 1));
            if !delay.is_zero() {
                assert!(q.expire_pending(now).is_none());
            }
            assert_eq!(q.expire_pending(now + delay), Some((1, unordered, 24)));
            assert_eq!(q.pending_bytes(), 8);
            assert!(q.expire_pending(now + delay).is_none());
            let mut tsn = 99;
            let fresh = q.send_next(&mut tsn, now + delay).unwrap();
            assert_eq!(fresh.tsn, 99);
            assert_eq!(tsn, 100);
            assert_eq!(
                fresh.stream_sequence_number,
                if unordered { 0 } else { u16::MAX }
            );
        }
    }
    for exemption in 0..3 {
        let mut q = OutboundQueue::default();
        let mut chunks = message(1, false, 2);
        for c in &mut chunks {
            c.deadline = Some(now);
            if exemption == 0 {
                c.payload_type = crate::PayloadProtocolIdentifier::Dcep;
            }
            if exemption == 1 {
                c.user_data.clear();
            }
        }
        q.push(chunks);
        if exemption == 2 {
            q.send_next(&mut 10, now).unwrap();
        }
        assert!(q.expire_pending(now).is_none());
    }
}
