# [M] Quest tree audit

## Goal

The quest tree agrees with itself and with `main`: done, stale,
conflicting, and misranked quests are fixed.

## Plan

Recurs once per major release (decided 2026-10-19). The PR that finishes
this run also adds the next one with the same file name at the end of the next
milestone's `Required` (m0 to m1, m1 to m2). The successor keeps the Goal
and this paragraph; the rest of its Plan is rewritten with what this run
learned: baselines, results, and what to check first. A successor placed in m2
also lists the [release cut](/quest/m2/release/cut.md) under `Required`, so it
waits for the release in flight to ship. Only `/quest-delete` ends the
recurrence. Fixes land in the run's PR, one commit each, rather than as follow-ups.
Only a change that needs a maintainer decision (public API, scope) or is
independently landable and too large to review alongside the rest becomes a
follow-up through `/quest-plan`.

Run `/quest-audit`. After a release cut shifts the milestones, also check
that the move left no broken links or misplaced quests. Until now, audits ran
ad hoc (2026-09-30, 10-05, 10-08, 10-10).

Checkpoint, 2026-10-19, against `main` at `2e1ec2ebf`:

- Read all 438 documents: m0, m3, and global structure in the coordinating
  audit, with parallel reviewers for all 133 m1 and 222 m2 documents.
  Structural validation found no broken links, cycles, or prerequisites in
  a later milestone.
- Retired datagram priority: #5770 (`79005b06d`) and #5872 (`8e3966cd3`)
  already contain the scheduler, publisher integration, saturation and
  eviction tests, fan-out benchmarks, and draft updates. Broadcast-domain
  exposure remains owned by track-priority-scope.
- Encoded the already-decided timestamp and E2EE naming-design
  prerequisites. Corrected stale paths, epoch discovery, accepted refresh
  recovery behavior, mobile priority, and the landed error, shard, benchmark
  site, and wire-accounting work. Cut-through now follows the bufferbloat
  policy.
- Four closed linked issues still have explicitly planned or in-flight
  work. Their closure alone was not treated as evidence to retire a quest.
  Open implementation PRs were not treated as merged work.
- Rechecked against `main` at `48c8a66dd` (#6017). The random-loss audit
  (#6000) landed its own BBR, cut-through, and loss-input verdicts, so its
  versions replace this run's; the 0-RTT window raise (#5937) also finished.
  Quests added since the baseline (the resume line, egress held state, and
  the request-behind-its-token race from #5972) were checked only against
  this run's edits, not audited in full.

This run remains open, with no successor, until the unresolved conflicts are
settled. The unattended instruction was to skip uncertain choices:

- [Auth issuer](/quest/m1/auth/issuer.md) specifies a shared handle and one
  live session's grant, but not how independent concurrent sessions select
  grants and refusals. Existing reconnect code only designates one active
  generation alongside draining predecessors.
- [Binding linger](/quest/m1/linger-ffi.md) extends the hand-written C ABI,
  while [generated C](/quest/m1/c/README.md) freezes it except claim safety.
- [Catalog HLS](/quest/m1/archive/catalog-hls.md) names `.hls.z` tracks,
  while [compression migration](/quest/m1/compress/migrate.md) removes `.z`
  names. Their shared timeline edits need a consistent final name and order.
- [Priority scope](/quest/m1/track-priority-scope.md) still preserves old
  archive priority encoding, while [archive](/quest/m1/archive/README.md)
  replaces the old layout without migration. The ladder also assumes a
  publisher-priority tiebreak that priority scope still asks to decide.
