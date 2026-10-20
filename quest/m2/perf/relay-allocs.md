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

Validate #5830's CPU profile mode with a real capture before relying on its
stacks. Sweep the fanout benchmark over publishers and subscribers so a cost
that grows with the table shows as a slope.

Decided 2026-10-17 (maintainer): m2, under perf.

Public API: none expected. Wire: none.
