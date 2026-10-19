//! How a connection answers frames a correct peer never sends

use super::*;

/// The wire value of a stream ID
fn id(initiator: Side, dir: Dir, index: u64) -> u64 {
    StreamId::new(initiator, dir, index).into()
}

/// Sends `frames` from the client on an established connection, returning how the server
/// closed, or `None` if it is still up
fn server_answer(frames: Frames) -> Option<TransportErrorCode> {
    let _guard = subscribe();
    let (mut pair, client_ch, server_ch) = connected();
    client_sends(&mut pair, client_ch, SpaceId::Data, frames);
    pair.drive();
    match lost(pair.server_conn_mut(server_ch)) {
        None => None,
        Some(ConnectionError::TransportError(error)) => Some(error.code),
        Some(other) => panic!("server lost with {other:?}"),
    }
}

/// Sends `frames` from the server on an established connection, returning how the client
/// closed, or `None` if it is still up
fn client_answer(frames: Frames) -> Option<TransportErrorCode> {
    let _guard = subscribe();
    let (mut pair, client_ch, server_ch) = connected();
    server_sends(&mut pair, server_ch, SpaceId::Data, frames);
    pair.drive();
    match lost(pair.client_conn_mut(client_ch)) {
        None => None,
        Some(ConnectionError::TransportError(error)) => Some(error.code),
        Some(other) => panic!("client lost with {other:?}"),
    }
}

#[test]
fn crypto_in_0rtt() {
    let _guard = subscribe();
    let mut pair = pair_with(|_| {});
    let config = client_with(|_| {});

    // A first connection leaves the client a ticket for 0-RTT
    let client_ch = pair.begin_connect(config.clone());
    pair.drive();
    pair.server.assert_accept();
    let now = pair.time;
    pair.client_conn_mut(client_ch)
        .close(now, VarInt(0), Bytes::new());
    pair.drive();

    let client_ch = pair.begin_connect(config);
    assert!(pair.client_conn_mut(client_ch).has_0rtt());
    pair.drive_client();
    client_sends(
        &mut pair,
        client_ch,
        SpaceId::Data,
        Frames::default().var(FrameType::CRYPTO, &[0, 1]).raw(&[0]),
    );
    pair.drive();

    let server_ch = pair.server.assert_accept();
    assert_eq!(
        lost_code(pair.server_conn_mut(server_ch)),
        TransportErrorCode::PROTOCOL_VIOLATION
    );
}

#[test]
fn stream_data_blocked_on_send_only() {
    let frames = Frames::default().var(
        FrameType::STREAM_DATA_BLOCKED,
        &[id(Side::Server, Dir::Uni, 0), 0],
    );
    assert_eq!(
        server_answer(frames),
        Some(TransportErrorCode::STREAM_STATE_ERROR)
    );
}

/// STREAM_DATA_BLOCKED is only logged, so a stream ID past the limit or never opened is ignored
#[test]
fn stream_data_blocked_on_unopened() {
    for id in [
        id(Side::Client, Dir::Bi, 1_000_000),
        id(Side::Server, Dir::Bi, 0),
    ] {
        let frames = Frames::default().var(FrameType::STREAM_DATA_BLOCKED, &[id, 0]);
        assert_eq!(server_answer(frames), None);
    }
}

#[test]
fn max_stream_data_on_recv_only() {
    let frames = Frames::default().var(
        FrameType::MAX_STREAM_DATA,
        &[id(Side::Client, Dir::Uni, 0), 1000],
    );
    assert_eq!(
        server_answer(frames),
        Some(TransportErrorCode::STREAM_STATE_ERROR)
    );
}

#[test]
fn max_stream_data_on_unopened() {
    let frames = Frames::default().var(
        FrameType::MAX_STREAM_DATA,
        &[id(Side::Server, Dir::Bi, 0), 1000],
    );
    assert_eq!(
        server_answer(frames),
        Some(TransportErrorCode::STREAM_STATE_ERROR)
    );
}

#[test]
fn stop_sending_on_recv_only() {
    let frames =
        Frames::default().var(FrameType::STOP_SENDING, &[id(Side::Client, Dir::Uni, 0), 0]);
    assert_eq!(
        server_answer(frames),
        Some(TransportErrorCode::STREAM_STATE_ERROR)
    );
}

#[test]
fn stop_sending_on_unopened() {
    let frames = Frames::default().var(FrameType::STOP_SENDING, &[id(Side::Server, Dir::Bi, 0), 0]);
    assert_eq!(
        server_answer(frames),
        Some(TransportErrorCode::STREAM_STATE_ERROR)
    );
}

#[test]
fn streams_blocked_above_limit() {
    for ty in [
        FrameType::STREAMS_BLOCKED_BIDI,
        FrameType::STREAMS_BLOCKED_UNI,
    ] {
        // The largest representable limit is fine
        let frames = Frames::default().var(ty, &[1 << 60]);
        assert_eq!(server_answer(frames), None);

        let frames = Frames::default().var(ty, &[(1 << 60) + 1]);
        assert_eq!(
            server_answer(frames),
            Some(TransportErrorCode::FRAME_ENCODING_ERROR)
        );
    }
}

#[test]
fn max_streams_above_limit() {
    for ty in [FrameType::MAX_STREAMS_BIDI, FrameType::MAX_STREAMS_UNI] {
        let frames = Frames::default().var(ty, &[1 << 60]);
        assert_eq!(server_answer(frames), None);

        let frames = Frames::default().var(ty, &[(1 << 60) + 1]);
        assert_eq!(
            server_answer(frames),
            Some(TransportErrorCode::FRAME_ENCODING_ERROR)
        );
    }
}

/// A NEW_CONNECTION_ID frame with sequence `sequence`, retiring those before `retire_prior_to`
fn new_cid(frames: Frames, sequence: u64, retire_prior_to: u64) -> Frames {
    let mut cid = [0xab; 9];
    cid[0] = 8;
    cid[1..5].copy_from_slice(&(sequence as u32).to_be_bytes());
    frames
        .var(FrameType::NEW_CONNECTION_ID, &[sequence, retire_prior_to])
        .raw(&cid)
        .raw(&[sequence as u8; RESET_TOKEN_SIZE])
}

#[test]
fn new_connection_id_above_limit() {
    // The client's CIDs past the server's active_connection_id_limit, none retired
    let mut frames = Frames::default();
    for sequence in 1..=8 {
        frames = new_cid(frames, sequence, 0);
    }
    assert_eq!(
        server_answer(frames),
        Some(TransportErrorCode::CONNECTION_ID_LIMIT_ERROR)
    );
}

/// The frame decoder rejects it before the connection's own PROTOCOL_VIOLATION check runs
#[test]
fn new_connection_id_retires_unissued() {
    let frames = new_cid(Frames::default(), 2, 3);
    assert_eq!(
        server_answer(frames),
        Some(TransportErrorCode::FRAME_ENCODING_ERROR)
    );
}

#[test]
fn new_connection_id_retired_is_discarded() {
    // Retiring through sequence 4 then repeating sequence 1 discards the stale frame
    let frames = new_cid(new_cid(Frames::default(), 4, 4), 1, 0);
    assert_eq!(server_answer(frames), None);
}

#[test]
fn retire_connection_id_unissued() {
    let frames = Frames::default().var(FrameType::RETIRE_CONNECTION_ID, &[1_000]);
    assert_eq!(
        server_answer(frames),
        Some(TransportErrorCode::PROTOCOL_VIOLATION)
    );
}

/// Has the client retire the server CID its forged packet is sent to, and lets the server
/// answer with a replacement CID
fn retire_in_use(pair: &mut Pair, client_ch: ConnectionHandle, server_ch: ConnectionHandle) {
    let active = pair.client_conn_mut(client_ch).active_rem_cid_seq();
    client_sends(
        pair,
        client_ch,
        SpaceId::Data,
        Frames::default().var(FrameType::RETIRE_CONNECTION_ID, &[active]),
    );
    pair.drive_server();
    assert!(lost(pair.server_conn_mut(server_ch)).is_none());
}

/// RFC 9000 lets a receiver close when a packet retires its own destination CID; the server
/// instead retires it and issues a replacement, which the client, never having retired the CID
/// it still uses, rejects as over its limit
#[test]
fn retire_connection_id_in_use() {
    let _guard = subscribe();
    let (mut pair, client_ch, server_ch) = connected();
    retire_in_use(&mut pair, client_ch, server_ch);
    pair.drive_client();
    assert_eq!(
        lost_code(pair.client_conn_mut(client_ch)),
        TransportErrorCode::CONNECTION_ID_LIMIT_ERROR
    );
}

/// A connection closed by its own transport error sends CONNECTION_CLOSE to a CID the server
/// retired, and the server answers with a stateless reset, which must neither replace the unread
/// close reason with `Reset` nor, once the reason was read, report a second `ConnectionLost`.
fn stateless_reset_after_local_close(poll_early: bool) {
    let _guard = subscribe();
    let (mut pair, client_ch, server_ch) = connected();
    retire_in_use(&mut pair, client_ch, server_ch);
    pair.drive_client();
    if poll_early {
        assert_eq!(
            lost_code(pair.client_conn_mut(client_ch)),
            TransportErrorCode::CONNECTION_ID_LIMIT_ERROR
        );
    }

    pair.drive();
    let lost = lost(pair.client_conn_mut(client_ch));
    if poll_early {
        assert_eq!(lost, None);
    } else {
        assert_matches!(
            lost,
            Some(ConnectionError::TransportError(TransportError {
                code: TransportErrorCode::CONNECTION_ID_LIMIT_ERROR,
                ..
            }))
        );
    }
}

#[test]
fn stateless_reset_after_local_close_keeps_reason() {
    stateless_reset_after_local_close(false);
}

#[test]
fn stateless_reset_after_local_close_is_not_a_second_loss() {
    stateless_reset_after_local_close(true);
}

#[test]
fn new_token_from_client() {
    let frames = Frames::default()
        .var(FrameType::NEW_TOKEN, &[4])
        .raw(b"tokn");
    assert_eq!(
        server_answer(frames),
        Some(TransportErrorCode::PROTOCOL_VIOLATION)
    );
}

#[test]
fn new_token_empty() {
    let frames = Frames::default().var(FrameType::NEW_TOKEN, &[0]);
    assert_eq!(
        client_answer(frames),
        Some(TransportErrorCode::FRAME_ENCODING_ERROR)
    );
}

#[test]
fn handshake_done_from_client() {
    let frames = Frames::default().var(FrameType::HANDSHAKE_DONE, &[]);
    assert_eq!(
        server_answer(frames),
        Some(TransportErrorCode::PROTOCOL_VIOLATION)
    );
}

#[test]
fn stream_offset_at_max() {
    let max = VarInt::MAX.into_inner();
    let s = id(Side::Client, Dir::Uni, 0);

    // Ending at 2^62 - 1 is only past flow control, while ending past it is unrepresentable,
    // which RFC 9000 lets a receiver report as either error
    for offset in [max - 1, max] {
        let frames = Frames::default().stream(s, offset, b"x", false);
        assert_eq!(
            server_answer(frames),
            Some(TransportErrorCode::FLOW_CONTROL_ERROR)
        );
    }
}

#[test]
fn crypto_offset_at_max() {
    let max = VarInt::MAX.into_inner();
    let frames = Frames::default()
        .var(FrameType::CRYPTO, &[max, 1])
        .raw(b"x");
    assert_eq!(
        server_answer(frames),
        Some(TransportErrorCode::CRYPTO_BUFFER_EXCEEDED)
    );
}

#[test]
fn stream_data_past_final_size() {
    let s = id(Side::Client, Dir::Uni, 0);
    let frames = Frames::default().stream(s, 0, b"abc", true);
    let frames = frames.stream(s, 3, b"d", false);
    assert_eq!(
        server_answer(frames),
        Some(TransportErrorCode::FINAL_SIZE_ERROR)
    );
}

#[test]
fn stream_final_size_changed() {
    let s = id(Side::Client, Dir::Uni, 0);
    let frames = Frames::default().stream(s, 0, b"abc", true);
    let frames = frames.stream(s, 0, b"abcde", true);
    assert_eq!(
        server_answer(frames),
        Some(TransportErrorCode::FINAL_SIZE_ERROR)
    );
}

#[test]
fn stream_final_size_below_received() {
    let s = id(Side::Client, Dir::Uni, 0);
    let frames = Frames::default().stream(s, 0, b"abcde", false);
    let frames = frames.stream(s, 0, b"abc", true);
    assert_eq!(
        server_answer(frames),
        Some(TransportErrorCode::FINAL_SIZE_ERROR)
    );
}

#[test]
fn reset_final_size_below_received() {
    let s = id(Side::Client, Dir::Uni, 0);
    let frames = Frames::default().stream(s, 0, b"abcdefghij", false);
    let frames = frames.var(FrameType::RESET_STREAM, &[s, 0, 5]);
    assert_eq!(
        server_answer(frames),
        Some(TransportErrorCode::FINAL_SIZE_ERROR)
    );
}

#[test]
fn reset_final_size_differs_from_fin() {
    let s = id(Side::Client, Dir::Uni, 0);
    let frames = Frames::default().stream(s, 0, b"abc", true);
    let frames = frames.var(FrameType::RESET_STREAM, &[s, 0, 5]);
    assert_eq!(
        server_answer(frames),
        Some(TransportErrorCode::FINAL_SIZE_ERROR)
    );
}

#[test]
fn reset_final_size_past_flow_control() {
    let s = id(Side::Client, Dir::Uni, 0);
    let frames = Frames::default().var(FrameType::RESET_STREAM, &[s, 0, 1 << 40]);
    assert_eq!(
        server_answer(frames),
        Some(TransportErrorCode::FLOW_CONTROL_ERROR)
    );
}

#[test]
fn reset_on_send_only() {
    let frames = Frames::default().var(
        FrameType::RESET_STREAM,
        &[id(Side::Server, Dir::Uni, 0), 0, 0],
    );
    assert_eq!(
        server_answer(frames),
        Some(TransportErrorCode::STREAM_STATE_ERROR)
    );
}

#[test]
fn stream_on_send_only() {
    let frames = Frames::default().stream(id(Side::Server, Dir::Uni, 0), 0, b"x", false);
    assert_eq!(
        server_answer(frames),
        Some(TransportErrorCode::STREAM_STATE_ERROR)
    );
}

#[test]
fn stream_above_stream_limit() {
    let frames = Frames::default().stream(id(Side::Client, Dir::Uni, 1_000_000), 0, b"x", false);
    assert_eq!(
        server_answer(frames),
        Some(TransportErrorCode::STREAM_LIMIT_ERROR)
    );
}
