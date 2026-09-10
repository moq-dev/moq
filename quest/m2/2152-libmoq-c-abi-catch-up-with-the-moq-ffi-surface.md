# [M] libmoq: dynamic track serving and server-side accept

## Goal

A C embedder serves tracks on demand inside a broadcast it publishes, and
accepts sessions itself through the same two-phase SETUP the FFI exposes.
These are the two `moq-ffi` capabilities `rs/libmoq` still lacks that ride
the request records the origin dynamic handle already exposes.

## Plan

Most of the catch-up #2152 lists has landed in dev's `rs/libmoq/src/api.rs`:
subscription options, track info, abort codes, client TLS roots, datagrams
(`moq_datagram` :462-468, `moq_publish_track_datagram` :1948, and
`moq_consume_datagrams` with its read, free, and close :2599-2650), and raw
frame timestamps (:2538-2541). Two gaps remain:

- Dynamic track serving. moq-ffi's `MoqBroadcastProducer::requested_track`
  yields a `MoqTrackRequest` whose `accept(info)` returns the producer
  (rs/moq-ffi/src/producer.rs:483, :624); api.rs has no `requested` symbol.
  Mirror it as a callback-delivered request handle with accept and reject,
  on the datagram task's handle and terminal-status contract. Broadcast
  requests are not in this quest: `requested_broadcast` reaches C through
  the origin dynamic handle, so do not add a separate path here.
- Server-side accept. moq-ffi's `MoqServer::accept` yields a `MoqRequest`
  whose own `accept()` completes SETUP (rs/moq-ffi/src/server.rs:54, :144,
  :186, :259); a C embedder cannot accept sessions at all.

Each addition regenerates `moq.h`, touches `cpp/obs/src` only if used, and
updates `doc/lib/c/index.md`. That page's capability list (:39) already
claims dynamic tracks for C; the request handle makes it true.

Branch from `dev`. Fetch and the video format knob are additive,
so they ship on main through the related quest.

## Closes

- [#2152](https://github.com/moq-dev/moq/issues/2152) - close this issue when the quest finishes

## Related

- [libmoq fetch](/quest/m2/libmoq-fetch.md) - fetch_group and the video format knob, on main
