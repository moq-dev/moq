# [S] Broadcast race without a shared port

## Goal

moq-tokio `broadcast_race_quic_wins` binds no port it didn't reserve and
has a deterministic winner.

## Plan

It binds TCP `:0` and then UDP on the same number, which nothing reserves:
the collision [#4084](https://github.com/moq-dev/moq/pull/4084) removed from
its sibling after [#4055](https://github.com/moq-dev/moq/pull/4055) papered
over it with a retry.

The test shares one port only so both transports sit behind one URL.
Separate `:0` ports fix the bind collision but not the race itself: with
`websocket.delay = 0` either arm can legitimately win under load. Make the
order deterministic the way #4084 did, holding the WebSocket arm until QUIC
has connected, rather than asserting on a real race; no retry. #4084's
follow-ups (`tests/reconnect.rs` `spawn_server`, `tests/worker.rs`
`free_udp_port`) are the same probe-and-rebind pattern; fix them here if
cheap.
