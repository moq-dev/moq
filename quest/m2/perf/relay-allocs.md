# [M] The relay allocates less per delivered frame

## Goal

The fanout relay's heap churn per delivered frame drops measurably, starting
with the per-stream `Session::clone` and `SendStream::poll_write` sites, and
the heap profile tracks it so a regression shows.

## Plan

Measured by #5830's heap profile (`just bench-profile heap fanout-heavy 4`):
about 1.08M allocations/s, roughly 70 per delivered frame. Top churn:
`SendStream::poll_write` 41.5%, the web transport `Session::clone` 15.9%,
moq-net `SendStream` poll 10.3%, `Session` poll 8.2%,
`group::Consumer::with_recover` 5.9%, kio `Tasks<GroupServe>` 5.5%.

`moq-tokio`'s `poll_open_uni` clones the session and boxes a future per
opened stream, and the web `Session` holds header `Vec`s and optional
connect request/response, so each clone allocates. Start there.

A 2026-10-22 staging CDN SD profile (`f57c0aad`) put memory moves at 3.78%
self-time; `SendStream::poll_write` was the nearest resolved caller for 1.43%
of all samples. That copy alone does not justify a zero-copy rewrite; its
allocations are this quest's target.

The 2026-10-27 audit adds a site the profile should attribute:
`transport::boxed`'s `ErasedRecv::poll_lease` boxes each successful lease
(and `ErasedLease::chunks` its iterator), and frame ingest
(`Reader::poll_read_frame`) takes one chunk per lease, so a fragmented
payload pays a box per chunk. Keep the one-time session and stream erasure
that bounds monomorphization; only the per-lease box is the target.
Switching ingest to `poll_read_chunk` is not a fix, since it returns receive
credit before the segment seals. Its `memory` bench counted 34-49
allocations per frame over the in-memory transport at every frame size, so
most of the cost is fixed per frame, not per payload byte.

Validate #5830's CPU profile mode with a real capture before relying on its
stacks. Sweep the fanout benchmark over publishers and subscribers so a cost
that grows with the table shows as a slope.

Decided 2026-10-17 (maintainer): m2, under perf.

Decided 2026-10-22 (maintainer): this waits on the work that replaces the
session clones and boxed opens at the top of this profile; re-profile after
it lands before fixing what remains. Since 2026-10-23 the QUIC half of that
removal is [native WebTransport](/quest/m1/quic-api/native-web.md), so the
relay's QUIC profile waits on it.

Public API: none expected. Wire: none.

## Required

- [Native WebTransport](/quest/m1/quic-api/native-web.md) - removes the QUIC path's per-stream session clone and boxed future, so the profile is retaken after it
