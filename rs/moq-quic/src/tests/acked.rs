//! Tests for [`Checkpoint::acked`], which reports whether the peer acknowledged a send stream's prefix

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
) -> Result<bool, ClosedStream> {
    checkpoint.acked(pair.client_conn_mut(ch))
}

/// The prefix the client last reported acknowledged on `s`, draining its events
fn reported(pair: &mut Pair, ch: ConnectionHandle, s: StreamId) -> Option<u64> {
    let mut reported = None;
    while let Some(event) = pair.client_conn_mut(ch).poll() {
        if let Event::Stream(StreamEvent::Acked { id, offset }) = event
            && id == s
        {
            reported = Some(offset);
        }
    }
    reported
}

/// Transmit the client's pending packets and drop them
fn lose_client_packets(pair: &mut Pair) {
    pair.client.drive(pair.time, pair.server.addr);
    assert!(!pair.client.outbound.is_empty());
    pair.client.outbound.clear();
}

/// A checkpoint is acknowledged only once every byte before it is, and the event that reports it
/// fires only for a checkpoint someone polled.
#[test]
fn waits_for_contiguous_prefix() {
    let _guard = subscribe();
    let mut pair = pair(true);
    let (client_ch, server_ch) = pair.connect();
    let s = open(&mut pair, client_ch, server_ch);
    pair.drive();
    reported(&mut pair, client_ch, s);

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
    assert_eq!(acked(&mut pair, client_ch, &after), Ok(false));

    // The retransmission fills the gap, which acknowledges both at once
    pair.drive();
    assert_eq!(
        reported(&mut pair, client_ch, s),
        Some(b"headerlostafter".len() as u64)
    );
    assert_eq!(acked(&mut pair, client_ch, &lost), Ok(true));
    assert_eq!(acked(&mut pair, client_ch, &after), Ok(true));
    assert!(pair.client_conn_mut(client_ch).stats().path.lost_packets > 0);
}

/// Without a poll waiting on it, an acknowledgment is not reported.
#[test]
fn unpolled_is_not_reported() {
    let _guard = subscribe();
    let mut pair = pair(true);
    let (client_ch, server_ch) = pair.connect();
    let s = open(&mut pair, client_ch, server_ch);

    pair.client_send(client_ch, s).write(b"payload").unwrap();
    let checkpoint = pair.client_send(client_ch, s).checkpoint();
    pair.drive();
    assert_eq!(reported(&mut pair, client_ch, s), None);
    assert_eq!(acked(&mut pair, client_ch, &checkpoint), Ok(true));
}

/// A checkpoint taken before anything was written, or once everything was acknowledged, is
/// acknowledged at once.
#[test]
fn already_acked() {
    let _guard = subscribe();
    let mut pair = pair(true);
    let (client_ch, _) = pair.connect();
    let s = pair.client_streams(client_ch).open(Dir::Uni).unwrap();
    let empty = pair.client_send(client_ch, s).checkpoint();
    assert_eq!(acked(&mut pair, client_ch, &empty), Ok(true));

    pair.client_send(client_ch, s).write(b"payload").unwrap();
    pair.drive();
    let all = pair.client_send(client_ch, s).checkpoint();
    assert_eq!(acked(&mut pair, client_ch, &all), Ok(true));
}

/// A finished stream is freed by the ACK that completes it, which reports a waiting checkpoint
/// before the stream finishes.
#[test]
fn finished_stream_reports_before_freed() {
    let _guard = subscribe();
    let mut pair = pair(true);
    let (client_ch, server_ch) = pair.connect();
    let s = open(&mut pair, client_ch, server_ch);
    pair.drive();
    reported(&mut pair, client_ch, s);

    pair.client_send(client_ch, s).write(b"payload").unwrap();
    let checkpoint = pair.client_send(client_ch, s).checkpoint();
    assert_eq!(acked(&mut pair, client_ch, &checkpoint), Ok(false));
    pair.client_send(client_ch, s).finish().unwrap();
    pair.drive();

    assert_eq!(pair.client_streams(client_ch).send_streams(), 0);
    let mut events = Vec::new();
    while let Some(Event::Stream(event)) = pair.client_conn_mut(client_ch).poll() {
        events.push(event);
    }
    let len = b"headerpayload".len() as u64;
    assert_eq!(
        events,
        [
            StreamEvent::Acked { id: s, offset: len },
            StreamEvent::Finished { id: s }
        ]
    );
    assert_eq!(
        acked(&mut pair, client_ch, &checkpoint),
        Err(ClosedStream::default())
    );
}

/// A reset fails the checkpoints past its Reliable Size at once, while those before it are
/// still acknowledged once the prefix arrives.
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
    assert_eq!(acked(&mut pair, client_ch, &kept), Ok(false));

    // The retransmitted prefix is acknowledged before the reset frees the stream
    pair.drive();
    assert_eq!(pair.client_streams(client_ch).send_streams(), 0);
    assert_eq!(
        reported(&mut pair, client_ch, s),
        Some(b"headerkept".len() as u64)
    );
}

/// A peer that stopped reading will not read what it acknowledges, before or after the
/// checkpoint was taken.
#[test]
fn stop_sending_fails() {
    let _guard = subscribe();
    let mut pair = pair(true);
    let (client_ch, server_ch) = pair.connect();
    let s = open(&mut pair, client_ch, server_ch);

    pair.client_send(client_ch, s).write(b"unread").unwrap();
    let before = pair.client_send(client_ch, s).checkpoint();
    lose_client_packets(&mut pair);
    assert_eq!(acked(&mut pair, client_ch, &before), Ok(false));

    pair.server_recv(server_ch, s).stop(VarInt(7)).unwrap();
    pair.drive();
    assert_eq!(
        pair.client_send(client_ch, s).stopped(),
        Ok(Some(VarInt(7)))
    );
    let after = pair.client_send(client_ch, s).checkpoint();
    assert_eq!(reported(&mut pair, client_ch, s), None);
    for checkpoint in [&before, &after] {
        assert_eq!(
            acked(&mut pair, client_ch, checkpoint),
            Err(ClosedStream::default())
        );
    }
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
