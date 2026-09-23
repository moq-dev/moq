# [S] The io_uring workers report a session's peer address and SNI

## Goal

A session accepted by the io_uring QUIC workers reports `remote`, `local`, and
`server_name` in its `moq_auth::Request` like the tokio listener does, so an
auth server (moq.pro's session limits per remote address in particular) sees
the same facts whichever runtime accepted the session. Today the workers report
only the negotiated protocol.

## Plan

Unplanned. `moq_uring::quic::Connection` exposes `peer_chain()` and
`protocol()` but no peer address or handshake data. noq's
`Connection` has `remote_address()` and `handshake_data()`. Add
the accessors to the backend and the facade, then fill the request in
`rs/moq-relay/src/uring.rs` where the comment names this quest. Run
`/plan-quests` on this file.

## Related

- [Perf](/quest/m1/perf/README.md) - the io_uring line these workers belong to
