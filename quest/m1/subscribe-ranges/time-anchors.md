# [L] Time anchors

## Goal

A range start or end may name a presentation time: `Time(pts)` in the track's
timescale. A start resolves to the group containing pts, an end to just after that
group, and an untimed track refuses either. It lands in its own PR, before
[lite-07 finalizes](/quest/m1/lite07-finalize.md).

## Plan

Split out of #5929 (maintainer, 2026-10-19), which ships absolute and relative
anchors only. Start from the `quest/m1/subscribe-ranges/time-end` branch, which
holds that PR's time anchors through every binding: `Anchor::Time`, the
`Holdings` resolution in `Window`, the end settlement (`Holdings::after`,
`TrackState::accounted`), and the failover handling. Merge main into it and drop
what #5929 changed since.

Decide first whether time starts ship alone. A start resolves once from the
cache, but keeping it without time ends needs a start-only anchor type in every
binding, which is why #5929 dropped both.

Open findings from #5929's review, each needing a regression test:

- **Per-read rescan behind a peer-controlled gap** (Codex 4320985614). Settling an
  unresolved end walks back from the live edge on every unsettled read. A peer that
  leaves an early sequence missing and keeps sending later groups makes each read
  revisit a growing prefix until the gap's grace runs out. Keep an incremental
  settle boundary, so each read scans only what arrived since.
- **Failover replacing an unsettled end** (Codex 4320985627). A copy capped at a
  route switch reports an unsettled time end as unresolved, so the cap replaces
  the `Time` anchor for good, and a group starting before the time that arrives
  later on the old copy is delivered. Keep the time filter on the reader, separate
  from the copy cap.
- **One speculative group past an end.** An unresolved end covers every sequence
  through `u64::MAX`, so each group handed out before its first frame can leak past
  it at the live edge. Cap it at the newest held group known to start at or before
  pts, plus the first unstarted group above it, and recompute as groups start.
  Raising the cap must wake readers: register on the speculative group's start
  (`group::Producer::poll_started`) from both `Cursor::poll_recv_group` and
  `poll_next_group`. A held group that never shows a start (a copy joined
  mid-group) must not pin the cap. Datagrams are not in `lookup`, so on a
  datagram-only timed track an end never resolves: resolve datagram starts too,
  or leave datagram reads uncapped. Reproduced on #5929: range
  `time(3000)..time(3500)` with three groups opened before any frame delivers
  all three.

Also pass a time anchor the cache can't resolve to the `track::Dynamic` handler
in its range request, once [dynamic range requests](/quest/m1/subscribe-ranges/dynamic.md)
land, and update `quest/m1/binding-timestamps.md` for the anchor's
`timestamp_us` fields.

## Related

- [Dynamic range requests](/quest/m1/subscribe-ranges/dynamic.md) - range requests a `Dynamic` handler resolves
- [Archive line](/quest/m1/archive/README.md) - DVR seek, which needs time anchors
