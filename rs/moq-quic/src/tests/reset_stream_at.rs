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
) -> Result<(), CheckpointError> {
    checkpoint.reset(pair.client_conn_mut(ch), ERROR)
}

/// Transmit the client's pending packets and drop them
fn lose_client_packets(pair: &mut Pair) {
    pair.client.drive(pair.time, pair.server.addr);
    assert!(!pair.client.outbound.is_empty());
    pair.client.outbound.clear();
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

#[test]
fn unsupported_leaves_stream_open() {
    let _guard = subscribe();
    let mut pair = pair(false);
    let (client_ch, server_ch) = pair.connect();

    let s = pair.client_streams(client_ch).open(Dir::Uni).unwrap();
    let checkpoint = write(&mut pair, client_ch, s);
    assert_eq!(
        reset(&mut pair, client_ch, checkpoint),
        Err(CheckpointError::Unsupported)
    );
    pair.client_send(client_ch, s).finish().unwrap();
    pair.drive();

    assert_eq!(accept(&mut pair, server_ch), s);
    assert_eq!(read(&mut pair, server_ch, s), (MSG.to_vec(), Ok(())));
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
        Err(CheckpointError::ClosedStream)
    );
    assert!(pair.client_send(client_ch, s).reset(ERROR).is_err());
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
