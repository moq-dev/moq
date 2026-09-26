# [M] Measure and cut the fixed cost of relaying a group

## Goal

The fixed cost of relaying one small group to one viewer drops, measured as
allocations and time per viewer-group in the session benchmark, with no
change to what is delivered.

## Plan

`session_delivery_viewers` spends about 26 µs per viewer per 4-frame,
64-byte group over the in-memory transport, through a publisher session, a
relay origin, and a viewer session: ~420 µs at 16 viewers on every version
(2026-09-25, Apple M4; the 256-viewer point is too noisy on that machine to
quote). No single function dominates. An earlier Linux profile spread it over `kio` waiter registration and parking,
`TrackState::evict_expired_scan` (4-5%), `Waiter`'s lazily allocated shared
waker (`Once::call`, about 3%, so waiters are created per poll), and
malloc/free (10-12%).

`SESSION_ALLOCS=1` makes the session bench print allocations per
viewer-group. The largest source, a fresh `Arc<Waker>` whenever a partial
wake made `kio::Park` retire a still-registered waiter, is gone: kio now
keeps that waiter and skips re-registering on lists that still hold it
(2026-09: 228 to 122 allocations per viewer-group, 352 to 118 paced). Find
the next largest source from there. A measured no-win abandons the quest,
per this line's rules.

## Related

- [Owned decoding copies](/quest/m1/perf/coding-decode.md) - decode-side copies are part of the same per-group cost
