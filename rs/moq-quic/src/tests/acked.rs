//! Tests for [`Checkpoint::acked`], which reports when the peer acknowledged a send stream's prefix

use super::*;

const LATENCY: Duration = Duration::from_millis(10);

/// A pair with a one-way delay, whose server does or does not advertise `reset_stream_at`
fn pair(reset_stream_at: bool) -> Pair {
    let mut config = server_config();
    let mut transport = cubic_transport();
    transport.reset_stream_at(reset_stream_at);
    config.transport = Arc::new(transport);
    let mut pair = Pair::new(Default::default(), config);
    pair.latency = LATENCY;
    pair
}

/// Open a client stream that the server has accepted, with nothing acknowledged yet
fn open(pair: &mut Pair, client_ch: ConnectionHandle, server_ch: ConnectionHandle) -> StreamId {
    let s = pair.client_streams(client_ch).open(Dir::Uni).unwrap();
    pair.client_send(client_ch, s).write(b"header").unwrap();
    pair.drive();
    assert_eq!(pair.server_streams(server_ch).accept(Dir::Uni), Some(s));
    s
}

fn acked(
    pair: &mut Pair,
    ch: ConnectionHandle,
    checkpoint: &Checkpoint,
) -> Result<Option<Instant>, ClosedStream> {
    checkpoint.acked(pair.client_conn_mut(ch))
}

/// Whether the client reported a checkpoint on `s` resolving, draining its events
fn reported(pair: &mut Pair, ch: ConnectionHandle, s: StreamId) -> bool {
    let mut reported = false;
    while let Some(event) = pair.client_conn_mut(ch).poll() {
        reported |= matches!(event, Event::Stream(StreamEvent::Acked { id }) if id == s);
    }
    reported
}

/// Transmit the client's pending packets and drop them
fn lose_client_packets(pair: &mut Pair) {
    pair.client.drive(pair.time, pair.server.addr);
    assert!(!pair.client.outbound.is_empty());
    pair.client.outbound.clear();
}

/// The instant leaves out the peer's delayed-ACK timer, but keeps both one-way trips.
#[test]
fn excludes_ack_delay() {
    let _guard = subscribe();
    let mut pair = pair(true);
    let (client_ch, server_ch) = pair.connect();
    let s = open(&mut pair, client_ch, server_ch);
    reported(&mut pair, client_ch, s);

    // A lone packet waits out the peer's delayed-ACK timer
    pair.client_send(client_ch, s).write(b"payload").unwrap();
    let checkpoint = pair.client_send(client_ch, s).checkpoint();
    let sent = pair.time;
    assert_eq!(acked(&mut pair, client_ch, &checkpoint), Ok(None));
    let received = loop {
        let busy = pair.step();
        if acked(&mut pair, client_ch, &checkpoint) != Ok(None) {
            break pair.time;
        }
        assert!(busy, "never acknowledged");
    };
    let at = acked(&mut pair, client_ch, &checkpoint).unwrap().unwrap();

    assert!(reported(&mut pair, client_ch, s));
    assert_eq!(at - sent, 2 * LATENCY);
    assert!(
        received - at >= Duration::from_millis(20),
        "{:?}",
        received - at
    );
}

/// A reordered packet acknowledged after a later one keeps its receive instant: the ACK's delay
/// describes the later packet, so none is subtracted.
#[test]
fn reordered_ack_keeps_its_delay() {
    const HOLD: Duration = Duration::from_millis(1);
    let _guard = subscribe();
    let mut pair = pair(true);
    let (client_ch, server_ch) = pair.connect();
    let s = open(&mut pair, client_ch, server_ch);

    // The first packet is held back, so it arrives after the second
    pair.client_send(client_ch, s).write(b"held").unwrap();
    let held = pair.client_send(client_ch, s).checkpoint();
    pair.client.drive(pair.time, pair.server.addr);
    pair.client.delay_outbound();
    pair.client_send(client_ch, s).write(b"ahead").unwrap();
    pair.drive_client();

    pair.time += HOLD;
    let released = pair.time;
    pair.client.finish_delay();
    pair.drive_client();
    pair.drive();

    // The peer acknowledges it at once, as out of order, after it already acknowledged the
    // later packet
    let at = acked(&mut pair, client_ch, &held).unwrap().unwrap();
    assert_eq!(at - released, 2 * LATENCY);
}

/// A gap filled after a later packet keeps its receive instant, even when the first ACK of the
/// later packet was lost and the gap's ACK is the first to raise the largest.
#[test]
fn gap_acked_with_largest_keeps_its_delay() {
    const HOLD: Duration = Duration::from_millis(1);
    let _guard = subscribe();
    let mut pair = pair(true);
    let (client_ch, server_ch) = pair.connect();
    let s = open(&mut pair, client_ch, server_ch);

    // The first packet is held back, so it arrives after the second
    pair.client_send(client_ch, s).write(b"held").unwrap();
    let held = pair.client_send(client_ch, s).checkpoint();
    pair.client.drive(pair.time, pair.server.addr);
    pair.client.delay_outbound();
    pair.client_send(client_ch, s).write(b"ahead").unwrap();
    pair.drive_client();

    // The peer's ACK of the second packet alone is lost
    pair.time += LATENCY;
    pair.server.drive(pair.time, pair.client.addr);
    assert!(!pair.server.outbound.is_empty());
    pair.server.outbound.clear();

    pair.time += HOLD;
    let released = pair.time;
    pair.client.finish_delay();
    pair.drive_client();
    pair.drive();

    let at = acked(&mut pair, client_ch, &held).unwrap().unwrap();
    assert_eq!(at - released, 2 * LATENCY);
}

/// An ACK that confirms a raised max_ack_delay subtracts its delay up to the new cap, which the
/// peer already used to hold that ACK.
#[test]
fn ack_delay_cap_follows_ack_frequency() {
    let _guard = subscribe();
    let (mut pair, client_ch, server_ch) = setup_ack_frequency_test(Duration::from_millis(200));
    let s = open(&mut pair, client_ch, server_ch);
    let sent_ack_frequency = |pair: &mut Pair| {
        pair.client_conn_mut(client_ch)
            .stats()
            .frame_tx
            .ack_frequency
    };
    let before = sent_ack_frequency(&mut pair);

    // The RTT jumps, so the client asks for a longer max_ack_delay than the default 25 ms
    pair.latency *= 100;
    pair.client_conn_mut(client_ch).ping();
    while sent_ack_frequency(&mut pair) == before {
        assert!(pair.step(), "never sent ACK_FREQUENCY");
    }

    // The peer holds the ACK covering both packets for the new max_ack_delay
    pair.client_send(client_ch, s).write(b"payload").unwrap();
    let checkpoint = pair.client_send(client_ch, s).checkpoint();
    let sent = pair.time;
    let received = loop {
        let busy = pair.step();
        if acked(&mut pair, client_ch, &checkpoint) != Ok(None) {
            break pair.time;
        }
        assert!(busy, "never acknowledged");
    };
    let at = acked(&mut pair, client_ch, &checkpoint).unwrap().unwrap();

    assert!(
        received - at > Duration::from_millis(25),
        "{:?}",
        received - at
    );
    // Pacing may hold the packet briefly after the write
    let trip = at - sent;
    assert!(
        trip >= 2 * pair.latency && trip < 2 * pair.latency + Duration::from_millis(1),
        "{trip:?}"
    );
}

/// A checkpoint resolves only once every byte before it is acknowledged, by the ACK that fills
/// the gap, however much of the stream that ACK covers.
#[test]
fn waits_for_contiguous_prefix() {
    let _guard = subscribe();
    let mut pair = pair(true);
    let (client_ch, server_ch) = pair.connect();
    let s = open(&mut pair, client_ch, server_ch);

    pair.client_send(client_ch, s).write(b"lost").unwrap();
    let lost = pair.client_send(client_ch, s).checkpoint();
    lose_client_packets(&mut pair);

    // Acknowledged, but behind the lost packet: the middle of the stream, not its prefix
    pair.client_send(client_ch, s).write(b"after").unwrap();
    let after = pair.client_send(client_ch, s).checkpoint();
    let acks = pair.client_conn_mut(client_ch).stats().frame_rx.acks;
    while pair.client_conn_mut(client_ch).stats().frame_rx.acks == acks {
        assert!(pair.step(), "never acknowledged");
    }
    assert_eq!(acked(&mut pair, client_ch, &lost), Ok(None));
    assert_eq!(acked(&mut pair, client_ch, &after), Ok(None));

    // The retransmission fills the gap, which resolves both at once
    pair.drive();
    let at = acked(&mut pair, client_ch, &lost).unwrap();
    assert!(at.is_some());
    assert_eq!(acked(&mut pair, client_ch, &after), Ok(at));
    assert!(reported(&mut pair, client_ch, s));
    assert!(pair.client_conn_mut(client_ch).stats().path.lost_packets > 0);
}

/// A checkpoint taken once everything is acknowledged resolves at once, with the ACK that got
/// there.
#[test]
fn already_acked() {
    let _guard = subscribe();
    let mut pair = pair(true);
    let (client_ch, server_ch) = pair.connect();
    let s = open(&mut pair, client_ch, server_ch);

    let first = pair.client_send(client_ch, s).checkpoint();
    let second = pair.client_send(client_ch, s).checkpoint();
    let at = acked(&mut pair, client_ch, &first).unwrap();
    assert!(at.is_some());
    assert_eq!(acked(&mut pair, client_ch, &second), Ok(at));
}

/// A finished stream is gone once acknowledged, but its checkpoints keep their answer.
#[test]
fn outlives_finished_stream() {
    let _guard = subscribe();
    let mut pair = pair(true);
    let (client_ch, server_ch) = pair.connect();
    let s = open(&mut pair, client_ch, server_ch);

    pair.client_send(client_ch, s).write(b"payload").unwrap();
    let checkpoint = pair.client_send(client_ch, s).checkpoint();
    pair.client_send(client_ch, s).finish().unwrap();
    pair.drive();

    assert_eq!(pair.client_streams(client_ch).send_streams(), 0);
    assert!(acked(&mut pair, client_ch, &checkpoint).unwrap().is_some());
    assert!(reported(&mut pair, client_ch, s));
}

/// A reset fails the checkpoints past its Reliable Size at once, while those before it still
/// resolve once the prefix arrives.
#[test]
fn reset_fails_past_reliable_size() {
    let _guard = subscribe();
    let mut pair = pair(true);
    let (client_ch, server_ch) = pair.connect();
    let s = open(&mut pair, client_ch, server_ch);

    pair.client_send(client_ch, s).write(b"kept").unwrap();
    let kept = pair.client_send(client_ch, s).checkpoint();
    let reset = pair.client_send(client_ch, s).checkpoint();
    pair.client_send(client_ch, s).write(b"dropped").unwrap();
    let dropped = pair.client_send(client_ch, s).checkpoint();
    lose_client_packets(&mut pair);
    reported(&mut pair, client_ch, s);

    reset
        .reset(pair.client_conn_mut(client_ch), VarInt(42))
        .unwrap();
    assert_eq!(
        acked(&mut pair, client_ch, &dropped),
        Err(ClosedStream::default())
    );
    assert_eq!(acked(&mut pair, client_ch, &kept), Ok(None));
    assert!(reported(&mut pair, client_ch, s));

    pair.drive();
    assert_eq!(pair.client_streams(client_ch).send_streams(), 0);
    assert!(acked(&mut pair, client_ch, &kept).unwrap().is_some());
}

/// A peer that stopped reading will not read what it acknowledges.
#[test]
fn stop_sending_fails() {
    let _guard = subscribe();
    let mut pair = pair(true);
    let (client_ch, server_ch) = pair.connect();
    let s = open(&mut pair, client_ch, server_ch);

    pair.client_send(client_ch, s).write(b"unread").unwrap();
    let checkpoint = pair.client_send(client_ch, s).checkpoint();
    lose_client_packets(&mut pair);
    reported(&mut pair, client_ch, s);

    pair.server_recv(server_ch, s).stop(VarInt(7)).unwrap();
    pair.drive();
    assert_eq!(
        acked(&mut pair, client_ch, &checkpoint),
        Err(ClosedStream::default())
    );
    assert!(reported(&mut pair, client_ch, s));
}

/// A checkpoint taken after the peer stopped reading fails at once, though data in flight is
/// still acknowledged.
#[test]
fn checkpoint_after_stop_sending_fails() {
    let _guard = subscribe();
    let mut pair = pair(true);
    let (client_ch, server_ch) = pair.connect();
    let s = open(&mut pair, client_ch, server_ch);

    pair.client_send(client_ch, s).write(b"in flight").unwrap();
    pair.server_recv(server_ch, s).stop(VarInt(7)).unwrap();
    pair.drive();
    assert_eq!(
        pair.client_send(client_ch, s).stopped(),
        Ok(Some(VarInt(7)))
    );

    let checkpoint = pair.client_send(client_ch, s).checkpoint();
    pair.drive();
    assert_eq!(
        acked(&mut pair, client_ch, &checkpoint),
        Err(ClosedStream::default())
    );
}

/// A checkpoint taken after a reset fails at once, though the reset still delivers its prefix.
#[test]
fn checkpoint_after_reset_fails() {
    let _guard = subscribe();
    let mut pair = pair(true);
    let (client_ch, server_ch) = pair.connect();
    let s = open(&mut pair, client_ch, server_ch);

    pair.client_send(client_ch, s).write(b"prefix").unwrap();
    let reset = pair.client_send(client_ch, s).checkpoint();
    lose_client_packets(&mut pair);
    reset
        .reset(pair.client_conn_mut(client_ch), VarInt(42))
        .unwrap();

    let checkpoint = pair.client_send(client_ch, s).checkpoint();
    pair.drive();
    assert_eq!(
        acked(&mut pair, client_ch, &checkpoint),
        Err(ClosedStream::default())
    );
}

/// Nothing is acknowledged on a closed connection.
#[test]
fn close_fails() {
    let _guard = subscribe();
    let mut pair = pair(true);
    let (client_ch, server_ch) = pair.connect();
    let s = open(&mut pair, client_ch, server_ch);

    pair.client_send(client_ch, s).write(b"unacked").unwrap();
    let checkpoint = pair.client_send(client_ch, s).checkpoint();
    lose_client_packets(&mut pair);

    let now = pair.time;
    pair.client_conn_mut(client_ch)
        .close(now, VarInt(0), Bytes::new());
    assert_eq!(
        acked(&mut pair, client_ch, &checkpoint),
        Err(ClosedStream::default())
    );
}
