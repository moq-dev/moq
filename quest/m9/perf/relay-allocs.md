# [M] The relay allocates less per delivered frame

## Goal

The fanout relay's heap churn per delivered frame drops measurably, starting
from a fresh heap profile of the native poll transports, and the heap profile
tracks it so a regression shows.

## Plan

Re-profile first. #5830's heap profile (`just bench-profile heap fanout-heavy 4`)
measured about 1.08M allocations/s, roughly 70 per delivered frame, with
`SendStream::poll_write` 41.5%, the web transport `Session::clone` 15.9%,
moq-net `SendStream` poll 10.3%, `Session` poll 8.2%,
`group::Consumer::with_recover` 5.9%, and kio `Tasks<GroupServe>` 5.5%. Those
numbers predate [moq-dev/moq#7771](https://github.com/moq-dev/moq/pull/7771),
which replaced the async-to-poll adapters with native poll transports:
`moq-tokio`'s `poll_open_uni` (`rs/moq-tokio/src/quic/driver/connection.rs`)
no longer clones the session or boxes a future per opened stream. Fix what the
new profile shows, not the old list.

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

Moved to lab 2026-11-12 (maintainer, /quest-audit): an internal optimization with no public API or wire change.

Public API: none expected. Wire: none.
