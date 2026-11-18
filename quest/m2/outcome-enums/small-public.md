# [M] Four small public returns take the shape their callers use

## Goal

- `moq_net` `broadcast::Producer::create_track` and the fill `accept`s
  report an already-taken track or group as a value
  (`Result<Option<_>>`), not `Err(Error::Duplicate)`, so moq-mux's catalog
  producer and the ietf subscriber stop matching it. js/net's `createTrack`,
  which throws `duplicate track`, returns `undefined` the same way.
- `moq_net::transport::poll::SendStream::poll_acked` returns no `Acked`.
  Its one consumer (`coding/writer.rs`) treats `Sent` like `Delivered`,
  though `Sent` is documented as "not an acknowledgment": confirm the writer
  is right before folding them. Whether a transport reports acknowledgment
  at all is a capability decided once, not a per-call result.
- `moq_tokio::Connection::poll_status`, `status()`, and `Status` are gone.
  `Status` is a lossy copy of `Phase`: moq-gst reads `Migrating` as
  disconnected while the old session still serves. moq-bench and moq-gst
  move to `Monitor::state_changed` or `presence_changed`, as moq-ffi and
  libmoq already have.
- `moq_tokio::Server::local_addr` returns `Option<SocketAddr>` like its
  `websocket_local_addr` and `tcp_local_addr` siblings, so moq-relay stops
  matching `Err(NoBackend)`.

## Plan

From the 2026-11-18 enum-return audit (maintainer, /quest-plan); the shapes
above are the recommendations the epic's goal settled. Each is a mechanical
migration of its callers; find them with
`rg 'Error::Duplicate|Acked::|poll_status|Status::|local_addr\('`.

Public API: breaking in moq-net (`create_track`, the fill `accept`s, the
`poll::SendStream` trait, which every transport implements), js/net
(`createTrack`), and moq-tokio (`Status`, `poll_status`, `local_addr`).
Wire: none.
