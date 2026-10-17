//! Tests for RESET_STREAM_AT, which resets a stream but still delivers its prefix

use super::*;

const ERROR: VarInt = VarInt(42);
const MSG: &[u8] = b"header and payload";
const RELIABLE: usize = 6;

/// A pair whose server does or does not advertise `reset_stream_at`
fn pair(server_reset_stream_at: bool) -> Pair {
    let mut config = server_config();
    let mut transport = cubic_transport();
    transport.reset_stream_at(server_reset_stream_at);
    config.transport = Arc::new(transport);
    Pair::new(Default::default(), config)
}

/// Read a server stream until it ends, returning the bytes and how it ended
fn read(pair: &mut Pair, ch: ConnectionHandle, s: StreamId) -> (Vec<u8>, Result<(), ReadError>) {
    let mut recv = pair.server_recv(ch, s);
    let mut chunks = recv.read(true).unwrap();
    let mut data = Vec::new();
    let end = loop {
        match chunks.next(usize::MAX) {
            Ok(Some(chunk)) => data.extend_from_slice(&chunk.bytes),
            Ok(None) => break Ok(()),
            Err(e) => break Err(e),
        }
    };
    let _ = chunks.finalize();
    (data, end)
}

/// Accept the client's next unidirectional stream on the server
fn accept(pair: &mut Pair, ch: ConnectionHandle) -> StreamId {
    pair.server_streams(ch).accept(Dir::Uni).unwrap()
}

/// Write `MSG` to a client stream, with a checkpoint after its first `RELIABLE` bytes
fn write(pair: &mut Pair, ch: ConnectionHandle, s: StreamId) -> Checkpoint {
    pair.client_send(ch, s).write(&MSG[..RELIABLE]).unwrap();
    let checkpoint = pair.client_send(ch, s).checkpoint();
    pair.client_send(ch, s).write(&MSG[RELIABLE..]).unwrap();
    checkpoint
}

/// Reset a client stream, still delivering what was written before `checkpoint`
fn reset(
    pair: &mut Pair,
    ch: ConnectionHandle,
    checkpoint: Checkpoint,
) -> Result<(), ClosedStream> {
    checkpoint.reset(pair.client_conn_mut(ch), ERROR)
}

/// Drive the pair to idle while the server reads stream `s` as its data arrives, returning the
/// bytes read and how the stream ended
///
/// A plain RESET_STREAM lets the receiver discard data it has not read yet, so only an
/// application that keeps reading sees the prefix a peer without RESET_STREAM_AT is sent.
fn drive_reading(
    pair: &mut Pair,
    ch: ConnectionHandle,
    s: StreamId,
) -> (Vec<u8>, Result<(), ReadError>) {
    let mut accepted = false;
    let mut data = Vec::new();
    loop {
        let busy = pair.step();
        accepted = accepted || pair.server_streams(ch).accept(Dir::Uni) == Some(s);
        if accepted {
            let (read, end) = read(pair, ch, s);
            data.extend(read);
            if end != Err(ReadError::Blocked) {
                return (data, end);
            }
        }
        if !busy {
            return (data, Err(ReadError::Blocked));
        }
    }
}

/// How many RESET_STREAM or RESET_STREAM_AT frames the client has sent
fn resets_sent(pair: &mut Pair, ch: ConnectionHandle) -> u64 {
    pair.client_conn_mut(ch).stats().frame_tx.reset_stream
}

/// Transmit the client's pending packets and drop them
fn lose_client_packets(pair: &mut Pair) {
    pair.client.drive(pair.time, pair.server.addr);
    assert!(!pair.client.outbound.is_empty());
    pair.client.outbound.clear();
}

/// A peer that stops reading gets a zero-sized reset, and the prefix it will not read is never
/// retransmitted.
fn stop_sending_abandons_prefix(reset_stream_at: bool) {
    let _guard = subscribe();
    let mut pair = pair(reset_stream_at);
    let (client_ch, server_ch) = pair.connect();

    // The server learns of the stream from its first byte
    let s = pair.client_streams(client_ch).open(Dir::Uni).unwrap();
    pair.client_send(client_ch, s).write(&MSG[..1]).unwrap();
    pair.drive();
    assert_eq!(accept(&mut pair, server_ch), s);

    // The rest of the prefix is lost, and so is any reset sent for it
    pair.client_send(client_ch, s)
        .write(&MSG[1..RELIABLE])
        .unwrap();
    let checkpoint = pair.client_send(client_ch, s).checkpoint();
    pair.client_send(client_ch, s)
        .write(&MSG[RELIABLE..])
        .unwrap();
    lose_client_packets(&mut pair);
    reset(&mut pair, client_ch, checkpoint).unwrap();
    pair.client.drive(pair.time, pair.server.addr);
    pair.client.outbound.clear();
    let resets = resets_sent(&mut pair, client_ch);

    pair.server_recv(server_ch, s).stop(VarInt(7)).unwrap();
    let streams = pair.client_conn_mut(client_ch).stats().frame_tx.stream;
    pair.drive();

    assert_eq!(
        pair.client_conn_mut(client_ch).stats().frame_tx.stream,
        streams
    );
    assert!(resets_sent(&mut pair, client_ch) > resets);
    assert_eq!(pair.client_streams(client_ch).send_streams(), 0);
}

#[test]
fn stop_sending_abandons_prefix_supported() {
    stop_sending_abandons_prefix(true);
}

#[test]
fn stop_sending_abandons_prefix_unsupported() {
    stop_sending_abandons_prefix(false);
}

#[test]
fn delivers_prefix_before_error() {
    let _guard = subscribe();
    let mut pair = pair(true);
    let (client_ch, server_ch) = pair.connect();

    let s = pair.client_streams(client_ch).open(Dir::Uni).unwrap();
    let checkpoint = write(&mut pair, client_ch, s);
    reset(&mut pair, client_ch, checkpoint).unwrap();
    pair.drive();

    assert_eq!(accept(&mut pair, server_ch), s);
    assert_eq!(
        read(&mut pair, server_ch, s),
        (MSG[..RELIABLE].to_vec(), Err(ReadError::Reset(ERROR)))
    );
    // Both halves let go of the stream once the prefix and the reset are through
    assert_eq!(pair.client_streams(client_ch).send_streams(), 0);
    assert!(matches!(
        pair.server_recv(server_ch, s).read(true),
        Err(ReadableError::ClosedStream)
    ));
}

/// A stream reset before any of its prefix was sent, as a WebTransport stream that only wrote its
/// header, still reaches the peer.
#[test]
fn delivers_unsent_prefix() {
    let _guard = subscribe();
    let mut pair = pair(true);
    let (client_ch, server_ch) = pair.connect();

    let s = pair.client_streams(client_ch).open(Dir::Uni).unwrap();
    pair.client_send(client_ch, s)
        .write(&MSG[..RELIABLE])
        .unwrap();
    let checkpoint = pair.client_send(client_ch, s).checkpoint();
    reset(&mut pair, client_ch, checkpoint).unwrap();
    pair.drive();

    assert_eq!(accept(&mut pair, server_ch), s);
    assert_eq!(
        read(&mut pair, server_ch, s),
        (MSG[..RELIABLE].to_vec(), Err(ReadError::Reset(ERROR)))
    );
}

#[test]
fn retransmits_lost_prefix() {
    let _guard = subscribe();
    let mut pair = pair(true);
    let (client_ch, server_ch) = pair.connect();

    let s = pair.client_streams(client_ch).open(Dir::Uni).unwrap();
    let checkpoint = write(&mut pair, client_ch, s);
    lose_client_packets(&mut pair);
    reset(&mut pair, client_ch, checkpoint).unwrap();

    // The reset arrives first, and withholds the error until the prefix is retransmitted
    pair.drive_client();
    pair.drive_server();
    assert_eq!(accept(&mut pair, server_ch), s);
    assert_eq!(
        read(&mut pair, server_ch, s),
        (Vec::new(), Err(ReadError::Blocked))
    );
    assert_matches!(
        pair.server_conn_mut(server_ch)
            .recv_stream(s)
            .received_reset(),
        Ok(None)
    );

    pair.drive();
    assert_eq!(
        read(&mut pair, server_ch, s),
        (MSG[..RELIABLE].to_vec(), Err(ReadError::Reset(ERROR)))
    );
    assert_eq!(pair.client_streams(client_ch).send_streams(), 0);
}

#[test]
fn retransmits_lost_reset() {
    let _guard = subscribe();
    let mut pair = pair(true);
    let (client_ch, server_ch) = pair.connect();

    let s = pair.client_streams(client_ch).open(Dir::Uni).unwrap();
    let checkpoint = write(&mut pair, client_ch, s);
    pair.drive();
    reset(&mut pair, client_ch, checkpoint).unwrap();
    lose_client_packets(&mut pair);
    pair.drive();

    assert_eq!(accept(&mut pair, server_ch), s);
    assert_eq!(
        read(&mut pair, server_ch, s),
        (MSG[..RELIABLE].to_vec(), Err(ReadError::Reset(ERROR)))
    );
    assert_eq!(pair.client_streams(client_ch).send_streams(), 0);
}

/// Once the prefix has been read, the reset is observable without another read.
#[test]
fn received_reset_after_prefix() {
    let _guard = subscribe();
    let mut pair = pair(true);
    let (client_ch, server_ch) = pair.connect();

    let s = pair.client_streams(client_ch).open(Dir::Uni).unwrap();
    let checkpoint = write(&mut pair, client_ch, s);
    reset(&mut pair, client_ch, checkpoint).unwrap();
    pair.drive();

    assert_eq!(accept(&mut pair, server_ch), s);
    assert_matches!(
        pair.server_conn_mut(server_ch)
            .recv_stream(s)
            .received_reset(),
        Ok(None)
    );
    let mut recv = pair.server_recv(server_ch, s);
    let mut chunks = recv.read(true).unwrap();
    assert_matches!(chunks.next(RELIABLE), Ok(Some(chunk)) if chunk.bytes == MSG[..RELIABLE]);
    let _ = chunks.finalize();
    assert_matches!(
        pair.server_conn_mut(server_ch)
            .recv_stream(s)
            .received_reset(),
        Ok(Some(ERROR))
    );
}

#[test]
fn after_finish() {
    let _guard = subscribe();
    let mut pair = pair(true);
    let (client_ch, server_ch) = pair.connect();

    for reliable in [RELIABLE, MSG.len()] {
        let s = pair.client_streams(client_ch).open(Dir::Uni).unwrap();
        let mut checkpoint = write(&mut pair, client_ch, s);
        if reliable == MSG.len() {
            checkpoint = pair.client_send(client_ch, s).checkpoint();
        }
        pair.client_send(client_ch, s).finish().unwrap();
        reset(&mut pair, client_ch, checkpoint).unwrap();
        pair.drive();

        assert_eq!(accept(&mut pair, server_ch), s);
        assert_eq!(
            read(&mut pair, server_ch, s),
            (MSG[..reliable].to_vec(), Err(ReadError::Reset(ERROR)))
        );
    }
    assert_eq!(pair.client_streams(client_ch).send_streams(), 0);
}

/// The Final Size never exceeds the flow control the writes were held to.
#[test]
fn flow_control_blocked() {
    let _guard = subscribe();
    let mut config = server_config();
    let mut transport = cubic_transport();
    transport.stream_receive_window(10u32.into());
    config.transport = Arc::new(transport);
    let mut pair = Pair::new(Default::default(), config);
    let (client_ch, server_ch) = pair.connect();

    let s = pair.client_streams(client_ch).open(Dir::Uni).unwrap();
    pair.client_send(client_ch, s)
        .write(&MSG[..RELIABLE])
        .unwrap();
    let checkpoint = pair.client_send(client_ch, s).checkpoint();
    assert_eq!(
        pair.client_send(client_ch, s).write(&MSG[RELIABLE..]),
        Ok(10 - RELIABLE)
    );
    assert_eq!(
        pair.client_send(client_ch, s).write(MSG),
        Err(WriteError::Blocked)
    );
    reset(&mut pair, client_ch, checkpoint).unwrap();
    pair.drive();

    assert_eq!(accept(&mut pair, server_ch), s);
    assert_eq!(
        read(&mut pair, server_ch, s),
        (MSG[..RELIABLE].to_vec(), Err(ReadError::Reset(ERROR)))
    );
    assert!(!pair.server_conn_mut(server_ch).is_closed());
}

/// Without RESET_STREAM_AT, the plain RESET_STREAM waits until the peer has acknowledged the data
/// before the checkpoint, so the peer reads that data before the error.
#[test]
fn unsupported_holds_reset_until_acked() {
    let _guard = subscribe();
    let mut pair = pair(false);
    let (client_ch, server_ch) = pair.connect();

    let s = pair.client_streams(client_ch).open(Dir::Uni).unwrap();
    let checkpoint = write(&mut pair, client_ch, s);
    reset(&mut pair, client_ch, checkpoint).unwrap();

    // The first flight carries only the data before the checkpoint
    pair.drive_client();
    assert_eq!(resets_sent(&mut pair, client_ch), 0);
    assert_eq!(
        drive_reading(&mut pair, server_ch, s),
        (MSG[..RELIABLE].to_vec(), Err(ReadError::Reset(ERROR)))
    );
    assert_eq!(resets_sent(&mut pair, client_ch), 1);
    pair.drive();
    assert_eq!(pair.client_streams(client_ch).send_streams(), 0);
}

#[test]
fn unsupported_retransmits_lost_prefix() {
    let _guard = subscribe();
    let mut pair = pair(false);
    let (client_ch, server_ch) = pair.connect();

    let s = pair.client_streams(client_ch).open(Dir::Uni).unwrap();
    let checkpoint = write(&mut pair, client_ch, s);
    lose_client_packets(&mut pair);
    reset(&mut pair, client_ch, checkpoint).unwrap();

    assert_eq!(
        drive_reading(&mut pair, server_ch, s),
        (MSG[..RELIABLE].to_vec(), Err(ReadError::Reset(ERROR)))
    );
    assert_eq!(resets_sent(&mut pair, client_ch), 1);
    pair.drive();
    assert_eq!(pair.client_streams(client_ch).send_streams(), 0);
}

/// Data the peer already acknowledged needs no wait.
#[test]
fn unsupported_resets_at_once_when_acked() {
    let _guard = subscribe();
    let mut pair = pair(false);
    let (client_ch, server_ch) = pair.connect();

    let s = pair.client_streams(client_ch).open(Dir::Uni).unwrap();
    let checkpoint = write(&mut pair, client_ch, s);
    pair.drive();
    assert_eq!(accept(&mut pair, server_ch), s);
    assert_eq!(
        read(&mut pair, server_ch, s),
        (MSG.to_vec(), Err(ReadError::Blocked))
    );

    reset(&mut pair, client_ch, checkpoint).unwrap();
    pair.drive_client();
    assert_eq!(resets_sent(&mut pair, client_ch), 1);
    pair.drive();
    assert_eq!(
        read(&mut pair, server_ch, s),
        (Vec::new(), Err(ReadError::Reset(ERROR)))
    );
}

/// A finished stream reset while its reset is held ends in the reset, never as finished.
#[test]
fn unsupported_after_finish() {
    let _guard = subscribe();
    let mut pair = pair(false);
    let (client_ch, server_ch) = pair.connect();

    let s = pair.client_streams(client_ch).open(Dir::Uni).unwrap();
    let checkpoint = write(&mut pair, client_ch, s);
    pair.client_send(client_ch, s).finish().unwrap();
    reset(&mut pair, client_ch, checkpoint).unwrap();
    assert_eq!(
        pair.client_send(client_ch, s).finish(),
        Err(FinishError::ClosedStream)
    );
    assert_eq!(
        pair.client_send(client_ch, s).write(MSG),
        Err(WriteError::ClosedStream)
    );

    assert_eq!(
        drive_reading(&mut pair, server_ch, s),
        (MSG[..RELIABLE].to_vec(), Err(ReadError::Reset(ERROR)))
    );
    pair.drive();
    assert_eq!(pair.client_streams(client_ch).send_streams(), 0);
    while let Some(event) = pair.client_conn_mut(client_ch).poll() {
        assert!(!matches!(
            event,
            Event::Stream(StreamEvent::Finished { .. })
        ));
    }
}

#[test]
fn unsupported_repeated_reset_is_closed() {
    let _guard = subscribe();
    let mut pair = pair(false);
    let (client_ch, _) = pair.connect();

    let s = pair.client_streams(client_ch).open(Dir::Uni).unwrap();
    let checkpoint = write(&mut pair, client_ch, s);
    let again = pair.client_send(client_ch, s).checkpoint();
    lose_client_packets(&mut pair);
    reset(&mut pair, client_ch, checkpoint).unwrap();
    assert_eq!(
        reset(&mut pair, client_ch, again),
        Err(ClosedStream::default())
    );
    assert!(pair.client_send(client_ch, s).reset(ERROR).is_err());
}

/// A connection closed while a reset is held closes cleanly, without sending it.
#[test]
fn unsupported_close_while_held() {
    let _guard = subscribe();
    let mut pair = pair(false);
    let (client_ch, server_ch) = pair.connect();

    let s = pair.client_streams(client_ch).open(Dir::Uni).unwrap();
    let checkpoint = write(&mut pair, client_ch, s);
    lose_client_packets(&mut pair);
    reset(&mut pair, client_ch, checkpoint).unwrap();
    let now = pair.time;
    pair.client_conn_mut(client_ch)
        .close(now, VarInt(0), Bytes::new());
    pair.drive();

    assert_eq!(resets_sent(&mut pair, client_ch), 0);
    assert!(pair.client_conn_mut(client_ch).is_closed());
    assert!(pair.server_conn_mut(server_ch).is_closed());
}

/// The data abandoned past the checkpoint still counts against connection flow control until
/// the peer learns the final size, and then returns.
#[test]
fn unsupported_returns_flow_control() {
    let _guard = subscribe();
    let mut config = server_config();
    let mut transport = cubic_transport();
    transport.reset_stream_at(false);
    transport.receive_window((MSG.len() as u32).into());
    config.transport = Arc::new(transport);
    let mut pair = Pair::new(Default::default(), config);
    let (client_ch, server_ch) = pair.connect();

    let s = pair.client_streams(client_ch).open(Dir::Uni).unwrap();
    let checkpoint = write(&mut pair, client_ch, s);
    reset(&mut pair, client_ch, checkpoint).unwrap();
    let next = pair.client_streams(client_ch).open(Dir::Uni).unwrap();
    assert_eq!(
        pair.client_send(client_ch, next).write(MSG),
        Err(WriteError::Blocked)
    );

    assert_eq!(
        drive_reading(&mut pair, server_ch, s),
        (MSG[..RELIABLE].to_vec(), Err(ReadError::Reset(ERROR)))
    );
    pair.drive();
    assert_eq!(pair.client_send(client_ch, next).write(MSG), Ok(MSG.len()));
}

/// A zero Reliable Size is a plain RESET_STREAM, which needs no negotiation.
#[test]
fn unsupported_allows_zero() {
    let _guard = subscribe();
    let mut pair = pair(false);
    let (client_ch, server_ch) = pair.connect();

    let s = pair.client_streams(client_ch).open(Dir::Uni).unwrap();
    let checkpoint = pair.client_send(client_ch, s).checkpoint();
    pair.client_send(client_ch, s).write(MSG).unwrap();
    reset(&mut pair, client_ch, checkpoint).unwrap();
    pair.drive();

    assert_eq!(accept(&mut pair, server_ch), s);
    assert_eq!(
        read(&mut pair, server_ch, s),
        (Vec::new(), Err(ReadError::Reset(ERROR)))
    );
}

#[test]
fn repeated_reset_is_closed() {
    let _guard = subscribe();
    let mut pair = pair(true);
    let (client_ch, _) = pair.connect();

    let s = pair.client_streams(client_ch).open(Dir::Uni).unwrap();
    let checkpoint = write(&mut pair, client_ch, s);
    let again = pair.client_send(client_ch, s).checkpoint();
    reset(&mut pair, client_ch, checkpoint).unwrap();
    assert_eq!(
        reset(&mut pair, client_ch, again),
        Err(ClosedStream::default())
    );
    assert!(pair.client_send(client_ch, s).reset(ERROR).is_err());
}

/// A checkpoint taken in 0-RTT that the server rejects names a discarded stream, so it stays
/// closed even once the same stream id is opened again.
#[test]
fn zero_rtt_rejected_checkpoint_is_closed() {
    let _guard = subscribe();
    let server_config = ServerConfig::with_crypto(Arc::new(server_crypto_with_alpn(vec![
        "foo".into(),
        "bar".into(),
    ])));
    let mut pair = Pair::new(Arc::new(EndpointConfig::default()), server_config);
    let mut client_crypto = Arc::new(client_crypto_with_alpn(vec!["foo".into()]));

    let client_ch = pair.begin_connect(ClientConfig::new(client_crypto.clone()));
    pair.drive();
    pair.server.assert_accept();
    let now = pair.time;
    pair.client_conn_mut(client_ch)
        .close(now, VarInt(0), Bytes::new());
    pair.drive();
    pair.client.connections.clear();
    pair.server.connections.clear();

    // A changed ALPN makes the server reject the resumed session's 0-RTT
    let this = Arc::get_mut(&mut client_crypto).expect("QuicClientConfig is shared");
    let inner = Arc::get_mut(&mut this.inner).expect("QuicClientConfig.inner is shared");
    inner.alpn_protocols = vec!["bar".into()];
    let client_ch = pair.begin_connect(ClientConfig::new(client_crypto));
    assert!(pair.client_conn_mut(client_ch).has_0rtt());
    let s = pair.client_streams(client_ch).open(Dir::Uni).unwrap();
    let checkpoint = write(&mut pair, client_ch, s);
    pair.drive();
    assert!(!pair.client_conn_mut(client_ch).accepted_0rtt());

    let reopened = pair.client_streams(client_ch).open(Dir::Uni).unwrap();
    assert_eq!(reopened, s);
    pair.client_send(client_ch, reopened).write(MSG).unwrap();
    assert_eq!(
        reset(&mut pair, client_ch, checkpoint),
        Err(ClosedStream::default())
    );
    pair.client_send(client_ch, reopened).finish().unwrap();
    pair.drive();

    let server_ch = pair.server.assert_accept();
    assert_eq!(accept(&mut pair, server_ch), s);
    assert_eq!(read(&mut pair, server_ch, s), (MSG.to_vec(), Ok(())));
}

/// A checkpoint from another connection panics, even where that connection has a stream of the
/// same id holding at least as much data.
#[test]
#[should_panic(expected = "checkpoint used on another connection")]
fn checkpoint_from_another_connection() {
    let _guard = subscribe();
    let mut first = pair(true);
    let (first_ch, _) = first.connect();
    let mut second = pair(true);
    let (second_ch, _) = second.connect();

    let s = first.client_streams(first_ch).open(Dir::Uni).unwrap();
    let checkpoint = write(&mut first, first_ch, s);
    let other = second.client_streams(second_ch).open(Dir::Uni).unwrap();
    assert_eq!(other, s);
    second.client_send(second_ch, other).write(MSG).unwrap();
    let _ = reset(&mut second, second_ch, checkpoint);
}

/// A client remembers the server's support for 0-RTT, and may use it there.
#[test]
fn zero_rtt() {
    let _guard = subscribe();
    let mut pair = pair(true);
    pair.server.handle_incoming = Box::new(validate_incoming);
    let config = client_config();

    let client_ch = pair.begin_connect(config.clone());
    pair.drive();
    pair.server.assert_accept();
    pair.client
        .connections
        .get_mut(&client_ch)
        .unwrap()
        .close(pair.time, VarInt(0), [][..].into());
    pair.drive();

    pair.client.addr = SocketAddr::new(
        Ipv6Addr::LOCALHOST.into(),
        CLIENT_PORTS.lock().unwrap().next().unwrap(),
    );
    let client_ch = pair.begin_connect(config);
    assert!(pair.client_conn_mut(client_ch).has_0rtt());
    let s = pair.client_streams(client_ch).open(Dir::Uni).unwrap();
    let checkpoint = write(&mut pair, client_ch, s);
    reset(&mut pair, client_ch, checkpoint).unwrap();
    pair.drive();

    assert!(pair.client_conn_mut(client_ch).accepted_0rtt());
    let server_ch = pair.server.assert_accept();
    assert_eq!(accept(&mut pair, server_ch), s);
    assert_eq!(
        read(&mut pair, server_ch, s),
        (MSG[..RELIABLE].to_vec(), Err(ReadError::Reset(ERROR)))
    );
}
