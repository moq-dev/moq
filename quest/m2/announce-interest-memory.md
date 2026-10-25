# [M] Announce interest streams share one route table

## Goal

An announce-interest stream (lite ANNOUNCE_PLEASE or moq-transport
SUBSCRIBE_NAMESPACE) costs memory and CPU proportional to what it has in
flight, not to every route under its prefix. A subscriber opening thousands
of root-prefix streams on a relay holding N routes no longer makes it hold
thousands of times N entries, and a benchmark swept over streams and routes
shows per-stream cost flat in N.

## Plan

From the 2026-10-24 pre-cut audit at 151a529670. Source-inferred, not
measured; inherited (release's `lite/publisher.rs:659` has the same
per-stream `origin.announced()`).

Each stream runs `origin.announced()` (`rs/moq-net/src/lite/publisher.rs`
about line 968, with no `Slots` acquire), materializing its own
`TableCursor.current`, `OriginConsumerState` pending/due/delivered entries,
and the `AnnounceRun` live map (`model/origin.rs` `register_cursor` about
line 5075, `Consumer::announced` about 6116). On lite-05/06, `init()` also
encodes the whole initial set before the first flush. Only QUIC stream
credit bounds the count (`DEFAULT_MAX_STREAMS = 10_000`), so the worst case
is about 10k times N times a few hundred bytes. Incoming announces are
bounded by `session::Limits`; outgoing interest streams are not.

Bound it by design, not with a cap: cursors walk a shared, refcounted table
snapshot lazily, materializing only what is in flight and gated on stream
write credit; lite-05/06 encodes its initial set incrementally under
`poll_flush`, as lite-07 does. The IETF SUBSCRIBE_NAMESPACE handler follows
the same pattern and gets the same fix.

Public API: none. Wire: none.

## Related

- [Announced snapshots](/quest/m2/performance-survey/announced-snapshots.md) - the HTTP listing's copy of the same table
- [Cluster routing](/quest/m1/cluster-routing/README.md) - reshapes the route table this walks
