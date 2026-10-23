# [M] Bound retained datagram memory across tracks

## Goal

A publisher cannot multiply a small compressed input into retained datagram
payloads outside a shared memory budget. Cover Rust relays and JS readers,
preserving best-effort delivery and accounting for both compressed and plain
payloads in library-owned queues and in-flight work.

## Plan

P1 resource concern found at main 6369eca in the release audit. Rust's
`model/track.rs::push_datagram` retains up to 64 entries per track without
charging the cache pool. `insert_deflated_datagram` eagerly inflates up to
65,535 bytes and `model/datagram.rs::Stored` retains both representations.
The pool is documented for groups, so this is an unbudgeted retention path,
not a claim that its documented accounting includes datagrams. Compression
amplifies the existing count-only retention design.

A public-model reproduction used a 1,024-byte pool, created compressed tracks,
subscribed before appending 64 separately allocated 65,535-byte zero payloads
to each, then read every retained datagram. At 1, 16, and 64 tracks it recovered
4,194,240, 67,107,840, and 268,431,360 plain bytes while `pool.used()` stayed zero.
The wire form of that payload is only 78 raw-DEFLATE bytes after removing the
sync-flush trailer. Source tracing confirms received compressed datagrams use
the same ring; this reproduction did not flood a network session.

JS `js/net/src/track.ts::#insertDatagram` likewise eagerly realizes
`frame.payload` for compressed input and publishes into count-bounded reader
queues. That analogous path is source-confirmed, not independently measured;
JS does not expose Rust's cache-pool API.

Make datagram retention participate in explicit shared byte ownership, with
best-effort eviction/drop under pressure and correct release on replacement,
reader cancellation, and track shutdown. Account for library-owned aliases
without double charging. JS exposes bare `Uint8Array` payloads, so accounting
for arbitrary application-retained aliases would require a separate ownership
API decision, not a guarantee this quest can make with the current surface.
Do not substitute an arbitrary track-count refusal. Verify through
receive paths, and benchmark retained bytes and delivery cost across both
track count and queued datagrams, including highly compressible payloads and
slow readers. The current consumer is relay/browser compressed datagram
delivery; no wire change is expected. Review any necessary public budget API
with the maintainer before implementing it.

## Related

- [Compressed tracks](/quest/m1/compress/README.md) - shared compression behavior and fallback
