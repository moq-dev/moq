# [M] Flate tracks in every binding

## Goal

Opaque tracks, compressed per group or not, are published and consumed the
same way from every language, not only Rust and JS, and the bytes on the wire
are identical across all of them. A track published from a wrapper decodes in
the browser with `@moq/flate` and vice versa.

## Plan

Decided in the 2026-09-30 audit: collapse the line into this quest and follow
the `json` namespace pattern that [#4519](https://github.com/moq-dev/moq/pull/4519)
sets, rather than the per-wrapper siblings of the old bindings quest.

`moq-flate` and `@moq/flate` own the snapshot and stream track modes
([#4425](https://github.com/moq-dev/moq/pull/4425)), so the crate
already owns the per-group window a caller could otherwise desynchronize.
Bind those track modes, not the bare codec: a `frame()` call across the FFI
boundary invites the window desync the track modes exist to prevent.

The `MoqFlate*Producer` constructors landed in #4519. Reconciled 2026-10-10:
[Publish timestamps](/quest/m1/publish-timestamp.md) owns missing producer
wrappers, including Python and Go. This quest owns the remaining consumers
and interop coverage; it does not wait for unrelated FFI-shape children.
What remains:

- moq-ffi has no consume side for flate tracks. Add consumers mirroring the
  `json` ones so each wrapper can read what it writes.
- Add consumers to each wrapper's `flate` namespace, following `json` and
  the timestamp-preserving `next()` / `latest()` contract. Do not duplicate
  the producer work owned by publish-timestamp.
- Document in `doc/lib/{py,swift,kt,go,dart}` beside the JSON entry.
- Tests: a round trip in each wrapper that has tests, and one cross-language
  check that a wrapper-published group decodes with `@moq/flate`. Run
  `just test interop --all`.

Public API: additive on top of #4519. Wire, catalog, and relay: none.
Compression stays invisible to `moq-net`; a compressed track is announced,
routed, and cached like any other.

## Required

- [Data consumer timestamps](/quest/m1/data-consumer-timestamps.md) - establishes the consumer contract these new bindings use
