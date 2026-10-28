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

Run `/quest-audit` after the release cut. Read every outstanding quest with
parallel auditors, then verify source claims, active PR ownership, Required
ordering, and the latest maintainer decisions before changing plans.

Also reconcile merged follow-ups (decided 2026-10-22 from the release audit):
read the Follow-ups of every PR merged since the last release, and plan each
concrete remainder that has no quest, open PR, or recorded decline. The
release audit found seven such remainders across
[moq-dev/moq#6124](https://github.com/moq-dev/moq/pull/6124),
[moq-dev/moq#6153](https://github.com/moq-dev/moq/pull/6153),
[moq-dev/moq#6251](https://github.com/moq-dev/moq/pull/6251),
[moq-dev/moq#6243](https://github.com/moq-dev/moq/pull/6243),
[moq-dev/moq#4991](https://github.com/moq-dev/moq/pull/4991), and
[moq-dev/moq#5758](https://github.com/moq-dev/moq/pull/5758).

Baseline from the m1 run, 2026-10-22 UTC, against `main` at `bce217e063`:

- Read all 463 quest documents across m0-m3 and the root with parallel
  read-only support. Checked 315 referenced GitHub items and the open-PR
  inventory. Source checks were targeted to the factual claims; hardware
  and deployment acceptance was not inferred from old inventory text.
- Fixed timestamp provenance's rank before its IETF passthrough consumer,
  the generated-C exception for dynamic range fills, discovery's fixed 2s
  linger, and replacement linger's existing-reader semantics.
- Carried m1's settled compression migration into deferred plans: one
  compressed catalog/stats track, ordinary tracks for opaque bytes, no
  moq-flate or `moq-mux::binary` helper, authenticated logical compression
  in the e2ee wrapper, and bounded prefix reconstruction for front eviction.
- Refreshed partly landed benchmark/report baselines and explicit
  prerequisites for the JS encrypted-wrapper mirror and fMP4 ambisonics.
- The JS allocation replacement passed all 2,493 js/net tests under load.
  The OBS drain failure reproduced on unchanged main and passed with the
  existing close-tail PR; its quest keeps the remaining merged-result proof.
- No m2 implementation quest was started, no external-organization report
  was posted, and ambiguous remote claim branches were left intact.

Check first next run:

- A moved quest can still be owned by a PR under its old branch name. The
  redundant failover quest moved to m0 while
  [moq-dev/moq#6113](https://github.com/moq-dev/moq/pull/6113) retained its
  m1 branch. Search PR titles, bodies, and source scope as well as exact
  branch names before taking a claim. Do not infer abandonment from age.
- Refresh all linked open PRs before changing ownership or deleting a
  quest. At this baseline, the TRACK_STATUS retirement, silent-peer
  retirement, audio conformance, CI audit, and local gate each had owners;
  an overlapping cleanup should finish through that PR.
- Confirm the first scheduled size report after the Linux release-link
  fix, the required count of post-change audio-budget nights, shared FFI
  publication, and release-gate availability. Partial evidence does not
  clear an external-condition quest. Safari WebKit bug 319818 was still
  NEW with no fixed shipping version established.
- Recheck local gate and binding-cost plans together: Bun `--no-orphans`
  reaps newly spawned local descendants even after successful completion,
  while the CI bypass preserves persistent queue daemons. A warm local
  daemon policy needs an explicit choice; no dependency edge was invented.
- Re-run the transitive Required rank check, including prerequisites added
  within an epic, and compare plans against replaced designs before looking
  for new implementation work.

## Required

- [Release cut](/quest/m2/release/cut.md) - the release in flight ships before this run starts
