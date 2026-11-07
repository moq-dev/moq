use bytes::Bytes;
use sctp_proto::{Association, AssociationHandle, ClientConfig, DatagramEvent};
use sctp_proto::{Endpoint, EndpointConfig, Payload, PayloadProtocolIdentifier};
use sctp_proto::{ReliabilityType, TransportConfig, generate_snap_token};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

struct Peer {
    ep: Endpoint,
    assoc: Association,
    handle: AssociationHandle,
    addr: SocketAddr,
}
impl Peer {
    fn receive(&mut self, now: Instant, remote: SocketAddr, bytes: Bytes) {
        let (_, e) = self
            .ep
            .handle(now, remote, None, None, bytes)
            .expect("routable SCTP packet");
        match e {
            DatagramEvent::AssociationEvent(e) => self.assoc.handle_event(e),
            _ => panic!("unexpected association"),
        }
    }
    fn drain(&mut self, now: Instant) -> Vec<Bytes> {
        let mut out = Vec::new();
        loop {
            while let Some(e) = self.assoc.poll_endpoint_event() {
                if let Some(e) = self.ep.handle_event(self.handle, e) {
                    self.assoc.handle_event(e);
                }
            }
            while self.assoc.poll().is_some() {}
            let Some(t) = self
                .assoc
                .poll_transmit(now)
                .or_else(|| self.ep.poll_transmit())
            else {
                break;
            };
            match t.payload {
                Payload::RawEncode(p) => out.extend(p),
                _ => panic!("outgoing parse payload"),
            }
        }
        out
    }
    fn messages(&mut self) -> Vec<Vec<u8>> {
        let mut out = Vec::new();
        while let Some(chunks) = self.assoc.stream(0).unwrap().read().unwrap() {
            let mut data = vec![0; chunks.len()];
            chunks.read(&mut data).unwrap();
            out.push(data);
        }
        out
    }
}
fn pairs(reliability: ReliabilityType) -> (Peer, Peer) {
    let tc = Arc::new(TransportConfig::default());
    let ta = generate_snap_token(&tc).unwrap();
    let tb = generate_snap_token(&tc).unwrap();
    let addr_a: SocketAddr = "127.0.0.1:5000".parse().unwrap();
    let addr_b: SocketAddr = "127.0.0.1:5001".parse().unwrap();
    let mut ep_a = Endpoint::new(Arc::new(EndpointConfig::new()), None);
    let mut ep_b = Endpoint::new(Arc::new(EndpointConfig::new()), None);
    let (ha, mut a) = ep_a
        .connect(
            ClientConfig::new().with_snap(ta.clone(), tb.clone()),
            addr_b,
        )
        .unwrap();
    let (hb, mut b) = ep_b
        .connect(ClientConfig::new().with_snap(tb, ta), addr_a)
        .unwrap();
    for assoc in [&mut a, &mut b] {
        assoc
            .open_stream(0, PayloadProtocolIdentifier::Binary)
            .unwrap()
            .set_reliability_params(true, reliability, 0)
            .unwrap();
    }
    (
        Peer {
            ep: ep_a,
            assoc: a,
            handle: ha,
            addr: addr_a,
        },
        Peer {
            ep: ep_b,
            assoc: b,
            handle: hb,
            addr: addr_b,
        },
    )
}
fn chunks(bytes: &[u8]) -> Vec<(u8, Option<u32>)> {
    let mut result = Vec::new();
    let mut i = 12;
    while i + 4 <= bytes.len() {
        let ty = bytes[i];
        let len = u16::from_be_bytes(bytes[i + 2..i + 4].try_into().unwrap()) as usize;
        let seq = if [0, 3, 192].contains(&ty) && len >= 8 {
            Some(u32::from_be_bytes(bytes[i + 4..i + 8].try_into().unwrap()))
        } else {
            None
        };
        result.push((ty, seq));
        if len < 4 {
            break;
        }
        i += (len + 3) & !3;
    }
    result
}

fn simulate(reliability: ReliabilityType) {
    use std::collections::VecDeque;
    let (mut a, mut b) = pairs(reliability);
    let base = Instant::now();
    let mut ab = VecDeque::<(u64, Bytes)>::new();
    let mut ba = VecDeque::<(u64, Bytes)>::new();
    let mut counts = [[0usize; 3]; 15]; // DATA, FORWARD-TSN, SACK
    let mut sent = 0;
    let mut received = 0;
    for ms in 0..15000u64 {
        let now = base + Duration::from_millis(ms);
        a.assoc.handle_timeout(now);
        b.assoc.handle_timeout(now);
        if ms < 10000 && ms % 17 == 0 {
            a.assoc.stream(0).unwrap().write(b"game packet").unwrap();
            sent += 1;
        }
        loop {
            let mut did = false;
            if ab.front().is_some_and(|(due, _)| *due <= ms) {
                let (_, p) = ab.pop_front().unwrap();
                b.receive(now, a.addr, p);
                did = true;
            }
            for p in b.drain(now) {
                for (ty, _) in chunks(&p) {
                    if ty == 3 {
                        counts[ms as usize / 1000][2] += 1;
                    }
                }
                ba.push_back((ms + 50, p));
            }
            received += b.messages().len();
            if ba.front().is_some_and(|(due, _)| *due <= ms) {
                let (_, p) = ba.pop_front().unwrap();
                a.receive(now, b.addr, p);
                did = true;
            }
            for p in a.drain(now) {
                for (ty, _) in chunks(&p) {
                    match ty {
                        0 => counts[ms as usize / 1000][0] += 1,
                        192 => counts[ms as usize / 1000][1] += 1,
                        _ => {}
                    }
                }
                ab.push_back((ms + 50, p));
            }
            if !did {
                break;
            }
        }
    }
    println!(
        "mode={reliability:?}, sent={sent}, received={received}, counts DATA/FWD/SACK each second:"
    );
    for (i, c) in counts.iter().enumerate() {
        println!("{}: {:?}", i, c);
    }
    assert_eq!(
        sent, received,
        "zero-drop ordered IP path should deliver every original DATA"
    );
    assert!(
        counts.iter().all(|c| c[1] == 0),
        "unlost DATA must never generate FORWARD-TSN"
    );
    assert!(
        counts[14].iter().all(|n| *n == 0),
        "settles once source traffic stops"
    );
}
#[test]
fn zero_retransmits_does_not_amplify_acknowledgements() {
    simulate(ReliabilityType::Reliable);
    simulate(ReliabilityType::Rexmit);
}

#[test]
fn retransmit_limit_is_applied_when_recovery_is_needed() {
    for limit in [0, 1, 2] {
        let (mut a, mut b) = pairs(ReliabilityType::Rexmit);
        a.assoc
            .stream(0)
            .unwrap()
            .set_reliability_params(true, ReliabilityType::Rexmit, limit)
            .unwrap();
        let base = Instant::now();
        a.assoc
            .stream(0)
            .unwrap()
            .write(b"intentionally dropped")
            .unwrap();
        let mut data = 0;
        let mut forwarded = false;
        // Deliberately drop DATA and deliver control packets. Drive real timeout
        // recovery: maxRTX counts retransmissions, not the first transmission.
        for ms in 0..30000u64 {
            let now = base + Duration::from_millis(ms);
            a.assoc.handle_timeout(now);
            b.assoc.handle_timeout(now);
            for packet in a.drain(now) {
                let types = chunks(&packet);
                if types.iter().any(|(ty, _)| *ty == 0) {
                    data += 1;
                } else {
                    forwarded |= types.iter().any(|(ty, _)| *ty == 192);
                    b.receive(now, a.addr, packet);
                }
            }
            for packet in b.drain(now) {
                a.receive(now, b.addr, packet);
            }
        }
        assert_eq!(data, 1 + limit, "maxRTX={limit}");
        assert!(
            forwarded,
            "abandoned DATA must advance without another incoming SACK"
        );
    }
}

fn run(reorder: bool) {
    let (mut a, mut b) = pairs(ReliabilityType::Rexmit);
    let base = Instant::now();
    let mut transmissions = Vec::new();
    for (ms, name) in [(0, "first"), (10, "second"), (20, "held")] {
        a.assoc.stream(0).unwrap().write(name.as_bytes()).unwrap();
        let packets = a.drain(base + Duration::from_millis(ms));
        for p in &packets {
            println!("{ms}ms A send {name}: {:?}", chunks(p));
        }
        assert_eq!(packets.len(), 1);
        transmissions.push(packets[0].clone());
    }
    let mut sacks = Vec::new();
    for (i, ms) in [(0, 50), (1, 60)] {
        b.receive(
            base + Duration::from_millis(ms),
            a.addr,
            transmissions[i].clone(),
        );
        sacks.extend(b.drain(base + Duration::from_millis(ms)));
    }
    assert_eq!(b.messages(), vec![b"first".to_vec(), b"second".to_vec()]);
    if !reorder {
        b.receive(
            base + Duration::from_millis(70),
            a.addr,
            transmissions[2].clone(),
        );
    }
    assert!(!sacks.is_empty(), "initial data must generate SACK");
    for p in sacks {
        println!("110ms A receives {:?}", chunks(&p));
        a.receive(base + Duration::from_millis(110), b.addr, p);
    }
    let forwards = a.drain(base + Duration::from_millis(110));
    assert!(
        !forwards
            .iter()
            .any(|p| chunks(p).iter().any(|(ty, _)| *ty == 192)),
        "original DATA remains in flight until recovery is needed"
    );
    for p in forwards {
        println!("160ms B receives {:?}", chunks(&p));
        b.receive(base + Duration::from_millis(160), a.addr, p);
    }
    if reorder {
        println!(
            "170ms B receives delayed original DATA {:?}",
            chunks(&transmissions[2])
        );
        b.receive(
            base + Duration::from_millis(170),
            a.addr,
            transmissions[2].clone(),
        );
    }
    let got = b.messages();
    println!(
        "reorder={reorder}: held message delivered={} (no network packet was dropped)",
        !got.is_empty()
    );
    assert_eq!(got, vec![b"held".to_vec()]);
}

#[test]
fn delayed_original_data_is_not_abandoned_by_an_older_sack() {
    run(false);
    run(true);
}

#[test]
fn fragmented_zero_retransmits_abandons_pending_tails_and_releases_credit() {
    use std::collections::{HashMap, VecDeque};
    for unordered in [false, true] {
        for size in [3000, 24000] {
            let (mut a, mut b) = pairs(ReliabilityType::Rexmit);
            a.assoc
                .stream(0)
                .unwrap()
                .set_reliability_params(unordered, ReliabilityType::Rexmit, 0)
                .unwrap();
            let base = Instant::now();
            a.assoc.stream(0).unwrap().write(&vec![7; size]).unwrap();
            a.assoc
                .stream(0)
                .unwrap()
                .write(b"following message")
                .unwrap();
            let mut ab = VecDeque::<(u64, Bytes)>::new();
            let mut ba = VecDeque::<(u64, Bytes)>::new();
            let mut transmissions = HashMap::<u32, usize>::new();
            let mut dropped = false;
            let mut forwarded = false;
            let mut messages = Vec::new();
            for ms in 0..10000u64 {
                let now = base + Duration::from_millis(ms);
                a.assoc.handle_timeout(now);
                b.assoc.handle_timeout(now);
                while ab.front().is_some_and(|(due, _)| *due <= ms) {
                    let (_, packet) = ab.pop_front().unwrap();
                    b.receive(now, a.addr, packet);
                }
                for packet in b.drain(now) {
                    ba.push_back((ms + 50, packet));
                }
                messages.extend(b.messages());
                while ba.front().is_some_and(|(due, _)| *due <= ms) {
                    let (_, packet) = ba.pop_front().unwrap();
                    a.receive(now, b.addr, packet);
                }
                for packet in a.drain(now) {
                    let types = chunks(&packet);
                    for (ty, tsn) in &types {
                        if *ty == 0 {
                            *transmissions.entry(tsn.unwrap()).or_default() += 1;
                        }
                        forwarded |= *ty == 192;
                    }
                    if !dropped && types.iter().any(|(ty, _)| *ty == 0) {
                        dropped = true;
                    } else {
                        ab.push_back((ms + 50, packet));
                    }
                }
            }
            assert!(forwarded);
            assert!(
                transmissions.values().all(|n| *n == 1),
                "RTX0 retransmitted a fragment: size={size}, unordered={unordered}"
            );
            assert_eq!(
                messages,
                vec![b"following message".to_vec()],
                "size={size}, unordered={unordered}"
            );
            assert_eq!(
                a.assoc.stream(0).unwrap().buffered_amount().unwrap(),
                0,
                "abandonment must release both pending and acknowledged prefix bytes"
            );
        }
    }
}

#[test]
fn fragment_retry_limits_and_lost_forward_recovery() {
    use std::collections::{HashMap, VecDeque};
    for unordered in [false, true] {
        for size in [3000, 24000] {
            for limit in [0, 1, 2] {
                let (mut a, mut b) = pairs(ReliabilityType::Rexmit);
                a.assoc
                    .stream(0)
                    .unwrap()
                    .set_reliability_params(unordered, ReliabilityType::Rexmit, limit)
                    .unwrap();
                let base = Instant::now();
                a.assoc.stream(0).unwrap().write(&vec![7; size]).unwrap();
                a.assoc
                    .stream(0)
                    .unwrap()
                    .write(b"following message")
                    .unwrap();
                let mut ab = VecDeque::<(u64, Bytes)>::new();
                let mut ba = VecDeque::<(u64, Bytes)>::new();
                let mut transmissions = HashMap::<u32, usize>::new();
                let mut first_tsn = None;
                let mut forward_drops = 0;
                let mut messages = Vec::new();
                for ms in 0..90000u64 {
                    let now = base + Duration::from_millis(ms);
                    a.assoc.handle_timeout(now);
                    b.assoc.handle_timeout(now);
                    while ab.front().is_some_and(|(due, _)| *due <= ms) {
                        let (_, packet) = ab.pop_front().unwrap();
                        b.receive(now, a.addr, packet);
                    }
                    for packet in b.drain(now) {
                        ba.push_back((ms + 50, packet));
                    }
                    messages.extend(b.messages());
                    while ba.front().is_some_and(|(due, _)| *due <= ms) {
                        let (_, packet) = ba.pop_front().unwrap();
                        a.receive(now, b.addr, packet);
                    }
                    for packet in a.drain(now) {
                        let types = chunks(&packet);
                        for (ty, tsn) in &types {
                            if *ty == 0 {
                                first_tsn.get_or_insert(tsn.unwrap());
                                *transmissions.entry(tsn.unwrap()).or_default() += 1;
                            }
                        }
                        if types.iter().any(|(ty, tsn)| *ty == 0 && *tsn == first_tsn) {
                            continue;
                        }
                        if types.iter().any(|(ty, _)| *ty == 192) && forward_drops < 2 {
                            forward_drops += 1;
                            continue;
                        }
                        ab.push_back((ms + 50, packet));
                    }
                }
                assert_eq!(transmissions[&first_tsn.unwrap()], (1 + limit) as usize);
                assert!(transmissions.values().all(|n| *n <= (1 + limit) as usize));
                assert_eq!(forward_drops, 2);
                assert_eq!(messages, vec![b"following message".to_vec()]);
                assert_eq!(a.assoc.stream(0).unwrap().buffered_amount().unwrap(), 0);
            }
        }
    }
}

#[test]
fn lost_prefix_sacks_do_not_strand_the_next_ordered_message() {
    let (mut a, mut b) = pairs(ReliabilityType::Rexmit);
    a.assoc
        .stream(0)
        .unwrap()
        .set_reliability_params(false, ReliabilityType::Rexmit, 0)
        .unwrap();
    let now = Instant::now();
    a.assoc.stream(0).unwrap().write(&vec![7; 24000]).unwrap();
    a.assoc
        .stream(0)
        .unwrap()
        .write(b"following message")
        .unwrap();
    // Deliver every fragment allowed by the initial congestion window, but
    // lose the SACKs. The receiver holds an incomplete, contiguous prefix.
    for packet in a.drain(now) {
        b.receive(now, a.addr, packet);
    }
    assert!(b.messages().is_empty());
    assert!(!b.drain(now).is_empty());

    let mut received = Vec::new();
    let mut forwarded = false;
    for ms in 1..10000 {
        let now = now + Duration::from_millis(ms);
        a.assoc.handle_timeout(now);
        b.assoc.handle_timeout(now);
        for packet in a.drain(now) {
            forwarded |= chunks(&packet).iter().any(|(ty, _)| *ty == 192);
            b.receive(now, a.addr, packet);
        }
        for packet in b.drain(now) {
            // Lose prefix SACKs, including delayed ones. Once recovery starts,
            // deliver all traffic in both directions without further loss.
            if forwarded {
                a.receive(now, b.addr, packet);
            }
        }
        received.extend(b.messages());
    }
    assert!(forwarded, "the sender must abandon the message on timeout");
    assert_eq!(a.assoc.stream(0).unwrap().buffered_amount().unwrap(), 0);
    assert_eq!(received, vec![b"following message".to_vec()]);
}

#[test]
fn unordered_abandonment_preserves_next_ordered_message() {
    let (mut a, mut b) = pairs(ReliabilityType::Rexmit);
    let base = Instant::now();
    a.assoc.stream(0).unwrap().write(b"lost unordered").unwrap();
    let mut forwarded = false;
    for ms in 0..30000 {
        let now = base + Duration::from_millis(ms);
        a.assoc.handle_timeout(now);
        for packet in a.drain(now) {
            let types = chunks(&packet);
            if types.iter().any(|(ty, _)| *ty == 0) {
                continue;
            }
            forwarded |= types.iter().any(|(ty, _)| *ty == 192);
            b.receive(now, a.addr, packet);
        }
        for packet in b.drain(now) {
            a.receive(now, b.addr, packet);
        }
        if forwarded {
            a.assoc
                .stream(0)
                .unwrap()
                .set_reliability_params(false, ReliabilityType::Reliable, 0)
                .unwrap();
            a.assoc
                .stream(0)
                .unwrap()
                .write(b"ordered successor")
                .unwrap();
            for packet in a.drain(now) {
                b.receive(now, a.addr, packet);
            }
            assert_eq!(b.messages(), vec![b"ordered successor".to_vec()]);
            break;
        }
    }
    assert!(forwarded);
}

#[test]
fn timed_streams_accept_existing_write_methods_after_idle() {
    for unordered in [false, true] {
        for method in 0..5 {
            let (mut a, mut b) = pairs(ReliabilityType::Timed);
            a.assoc
                .stream(0)
                .unwrap()
                .set_reliability_params(unordered, ReliabilityType::Timed, 10)
                .unwrap();
            let base = Instant::now();
            assert!(a.drain(base).is_empty());
            let now = base + Duration::from_secs(3600);
            let data = Bytes::from_static(b"timed message");
            let mut stream = a.assoc.stream(0).unwrap();
            let written = match method {
                0 => stream.write(&data),
                1 => stream.write_with_ppi(&data, PayloadProtocolIdentifier::Binary),
                2 => stream.write_sctp(&data, PayloadProtocolIdentifier::Binary),
                3 => stream.write_chunk(&data),
                4 => stream.write_chunks(&mut [data.clone()]),
                _ => unreachable!(),
            };
            assert_eq!(written.unwrap(), data.len());
            for packet in a.drain(now) {
                b.receive(now, a.addr, packet);
            }
            assert_eq!(b.messages(), vec![data.to_vec()]);
        }
    }
}

#[test]
fn zero_lifetime_timed_writes_preserve_following_ordered_delivery() {
    for unordered in [false, true] {
        let (mut a, mut b) = pairs(ReliabilityType::Timed);
        a.assoc
            .stream(0)
            .unwrap()
            .set_reliability_params(unordered, ReliabilityType::Timed, 0)
            .unwrap();
        a.assoc.stream(0).unwrap().write(&vec![7; 3000]).unwrap();
        a.assoc
            .stream(0)
            .unwrap()
            .set_reliability_params(false, ReliabilityType::Reliable, 0)
            .unwrap();
        a.assoc
            .stream(0)
            .unwrap()
            .write(b"following message")
            .unwrap();
        let now = Instant::now();
        for packet in a.drain(now) {
            b.receive(now, a.addr, packet);
        }
        assert_eq!(b.messages(), vec![b"following message".to_vec()]);
        assert_eq!(
            a.assoc.stream(0).unwrap().buffered_amount().unwrap(),
            b"following message".len()
        );
    }
}

#[test]
fn fragmented_timed_messages_expire_through_loss_and_reordering() {
    use std::collections::{HashMap, VecDeque};
    for unordered in [false, true] {
        for size in [3000, 24000] {
            let (mut a, mut b) = pairs(ReliabilityType::Rexmit);
            a.assoc
                .stream(0)
                .unwrap()
                .set_reliability_params(unordered, ReliabilityType::Timed, 10)
                .unwrap();
            let base = Instant::now();
            a.assoc.stream(0).unwrap().write(&vec![7; size]).unwrap();
            a.assoc
                .stream(0)
                .unwrap()
                .set_reliability_params(unordered, ReliabilityType::Reliable, 0)
                .unwrap();
            a.assoc
                .stream(0)
                .unwrap()
                .write(b"following message")
                .unwrap();
            let mut ab = VecDeque::<(u64, Bytes)>::new();
            let mut ba = VecDeque::<(u64, Bytes)>::new();
            let mut transmissions = HashMap::<u32, usize>::new();
            let mut dropped = false;
            let mut dropped_forward = false;
            let mut forwarded = false;
            let mut messages = Vec::new();
            for ms in 0..10000u64 {
                let now = base + Duration::from_millis(ms);
                a.assoc.handle_timeout(now);
                b.assoc.handle_timeout(now);
                while ab.front().is_some_and(|(due, _)| *due <= ms) {
                    let (_, packet) = ab.pop_front().unwrap();
                    b.receive(now, a.addr, packet);
                }
                for packet in b.drain(now) {
                    ba.push_back((ms + 50, packet));
                }
                messages.extend(b.messages());
                while ba.front().is_some_and(|(due, _)| *due <= ms) {
                    let (_, packet) = ba.pop_front().unwrap();
                    a.receive(now, b.addr, packet);
                }
                // Reverse each outbound batch to exercise reordered fragments.
                for packet in a.drain(now).into_iter().rev() {
                    let types = chunks(&packet);
                    let mut offset = 12;
                    while offset + 4 <= packet.len() {
                        let length =
                            u16::from_be_bytes([packet[offset + 2], packet[offset + 3]]) as usize;
                        if packet[offset] == 0 && length > 16 && packet[offset + 16] == 7 {
                            let tsn = u32::from_be_bytes(
                                packet[offset + 4..offset + 8].try_into().unwrap(),
                            );
                            *transmissions.entry(tsn).or_default() += 1;
                            assert!(ms < 10, "expired timed DATA reached the wire at {ms}ms");
                        }
                        offset += (length + 3) & !3;
                    }
                    forwarded |= types.iter().any(|(ty, _)| *ty == 192);
                    if !dropped && types.iter().any(|(ty, _)| *ty == 0) {
                        dropped = true;
                    } else if !dropped_forward && types.iter().any(|(ty, _)| *ty == 192) {
                        dropped_forward = true;
                    } else {
                        ab.push_back((ms + 50, packet));
                    }
                }
            }
            assert!(forwarded && dropped_forward);
            assert!(
                transmissions.values().all(|n| *n == 1),
                "timed message retransmitted after expiry: size={size}, unordered={unordered}"
            );
            assert_eq!(
                messages,
                vec![b"following message".to_vec()],
                "size={size}, unordered={unordered}"
            );
            assert_eq!(
                a.assoc.stream(0).unwrap().buffered_amount().unwrap(),
                0,
                "abandonment must release both pending and acknowledged prefix bytes"
            );
        }
    }
}
