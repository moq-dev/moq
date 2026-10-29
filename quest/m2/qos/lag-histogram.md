# [L] Viewer lag histogram

## Goal

The relay's `moq-stats` egress side reports, per broadcast, how far behind
its viewers are according to the frames the network has acknowledged: a
byte-weighted cumulative `lag` histogram and `dropped` counters (the frames
written after the newest acknowledged one when a group stream ends without
delivering them), on the stats totals, a group's totals, and the prefix tracks
(`<path>/prefix.json`). A spliced subscription stays exact across a route
switch, and a closing subscription records its last partial interval.
Subscriptions on a backend that cannot see acknowledgements report neither
field: absent, never zero.

## Plan

Starting point: the branch `quest/m2/qos/lag-histogram` on
[t0ms's fork](https://github.com/t0ms/moq/tree/quest/m2/qos/lag-histogram),
head `9ed8b831f93f39565333a92d5dd4a5f24d2c3fc5`, from
[moq-dev/moq#6791](https://github.com/moq-dev/moq/pull/6791) (closed
unmerged 2026-10-26). It ports the #4133 sampler onto `main` with
caller-supplied time (`Registry::report(&self, report, now)`), an explicit
close path, route-copy feeds, untimed tracks recording nothing, the five open
#4133 findings fixed, docs, and tests. Its frontier moves only on an
acknowledged group FIN, which this quest replaces. The PR body has the
details of what it settled; bring it up to date with `main` first.

Decided 2026-10-26 (maintainer, on #6791), each with its reason:

- **Frame granularity, through the ACK hook.** Group granularity is rejected
  as a definition: a healthy zero-RTT viewer on 2 s groups reads 0.5 s to 5 s
  behind, and every unsubscribe, rendition switch, or session close charges
  the open group's whole content to `dropped`, so a healthy audience with
  churn reads a dropped baseline. The frontier instead moves at every
  acknowledged frame end through [poll_acked](/quest/m2/quic-ack-hook.md), so
  the sampler reads a frontier at most one frame stale. `dropped` counts only
  the frames written after the newest acknowledged one. This folds in the
  frontier half of the former starvation-frames quest; the delivery-delay
  histogram stays in [its own quest](/quest/m2/delivery-delay.md). Its sketch,
  to adjust as the code dictates: the group stream's writer counts the bytes
  it writes, each frame end records its offset and timestamp, and one waiter
  per stream polls the oldest pending offset, interleaved with later writes so
  a lagging ACK never stalls sending, retiring every frame at or below what is
  acknowledged.
- **No hook, no fields.** A subscription whose backend reports `poll_acked`
  unsupported (qmux, the browser transport) keeps no frontier and adds
  nothing to `lag` or `dropped`. A node serving only such viewers leaves both
  absent. Document this in `doc/concept/stats.md`.
- **The lag edge is the newest group's.** On the branch, `Produced.newest` is
  a lifetime `fetch_max`, so one far-future timestamp pins the edge and every
  later sample reads `inf`. Take the edge from the newest group instead (still
  a max within it, since B-frames present out of order), so a jump clears at
  the next group. Only the publisher's own broadcast is affected, so no
  refusal or clamp.

Decided on the branch, kept:

- The cache track's field is `produced: Produced`, mirroring `Delivered` and
  `group::Producer::produced()`.
- `dropped` is a `Content` (`bytes`, `frames`, `groups`, `datagrams`), the
  same type as `stale`, and a subset of the top-level payload counters.
  `datagrams` is always zero, since a datagram has no acknowledgement to
  miss. Like `lag`, it is written only when non-empty, so ingress, idle, and a
  group's per-path `paths` rows stay unchanged on the wire.

Revisit under frame granularity: the branch counts a stall in whole report
intervals and charges no further stall in the final sample. Both were
compromises for a frontier that moved once per group.

Write-path cost, measured on the branch (`perf stat` user-space instructions
per frame, the 300-minus-100-group difference so setup cancels, medians of
three; criterion wall time was unusable on a loaded host):

| Workload | `main` | branch, `Duration` stamp | branch head |
| --- | ---: | ---: | ---: |
| `write_frame`, 64 B | 994 | 1,064 (+7.0%) | 1,027 (+3.3%) |
| `write_frames` x32, 64 B | 417 | 523 (+25%) | 471 (+13%) |
| 16 KiB frame, 16 chunks | 12,957 | 14,304 (+10%) | 13,690 (+5.7%) |

What remains per timed write is one `fetch_max`, one release `fetch_add`, and
a u128 rescale. Store the raw track-scale value and rescale in the sampler
instead, and re-measure; the per-frame offset bookkeeping above adds to this
path too, so measure it with the same method.

Keep the histogram contract the dashboard reads: buckets keyed by upper edge
(`"50ms"` to `"5s"`, then `"inf"`, empty buckets omitted), cumulative and
monotonic so readers diff two samples and sum nodes bucket by bucket.

Tests cover what group granularity hid: groups written across report ticks
rather than whole between them, a peer acknowledging in bursts, a reset
mid-group counting only the unacknowledged frames as `dropped`, an
unsupported backend leaving both fields absent, and a far-future timestamp
clearing at the next group.

Public API: breaking in `moq-net` (`stats::Registry::report` takes the
drain's instant); additive `Traffic` fields and a new `stats::Histogram`,
re-exported by `moq-stats`. Wire: the stats JSON gains the egress fields.
Update `doc/concept/stats.md` and the stats section of
`doc/bin/relay/config.md`.

## Required

- [poll_acked on moq-net's send stream](/quest/m2/quic-ack-hook.md) - the
  per-frame acknowledgement the frontier moves on
