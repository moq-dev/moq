# MoQ questline

## Goal

Keep the repository's living work organized as visible, versioned quests,
grouped into milestones ordered by priority.

## Plan

m0 holds urgent work of any type: release-line maintenance, broken CI and
Nightly, and any fix that cannot wait for its milestone, breaking or not.
m1 holds this cycle's additive work: improvements and fixes that need no
semver-breaking release, so each ships with the next cut or as a `release`
patch when backported. m2 is the next breaking release: every change that
would need one if it landed right after the cut. The
[release cut](/quest/m3/release/cut.md) waits on m2, its m0 cut blockers, and
the pre-cut release reviews (not m1). m3 and m4
are later work split by priority: m3 holds what a named consumer waits on,
release execution, and bug fixes in shipped code; m4 holds tests, tooling,
benchmarks, optimizations, and features nobody waits on yet. m5 holds work
gated on the outside world, or speculative work with no named consumer.

Until the `0.22.0` cut (decided 2026-11-28, maintainer, /quest-run), m0 to m2
focus on the lite-07 freeze and the cut. A quest joins or stays in m0 to m2
only when the cut's `Required` or [lite-07 finalization](/quest/m2/lite07-finalize.md)
needs it, directly or transitively, or when it makes a semver-breaking API or
wire change, which cannot ship in a patch. m0 holds the cut's operational
blockers: green Nightly, Interop, and Video runs, the flakes that turn them
red, and the cut's other m0 `Required` entries. m2 holds the breaking changes
and the non-breaking work the cut or lite-07 finalization requires; a quest
stays in m2 only for a reason of its own, since the cut waiting on m2 is not
one. m1 holds only in-flight work open PRs claimed before the decision: a
claimed quest stays put, since `quest mv` refuses it, and what its PR leaves
moves to m3 once the claim ends. Every other quest, additive API included,
joins m3 and ships later in a patch version. (Rejected: moving only m1;
keeping all of m0 as an ops bucket.)

The `m9` lab milestone (created 2026-11-12 as `lab`, renamed `m9` 2026-11-13)
was deleted on 2026-11-26 (maintainer, pre-cut audit): almost all of its quests
were internal moq perf, benchmark, `io_uring`, simulation, and video-quality
work rather than separate-repo experiments, so its live quests moved to m4, its
speculative ones to m5, and its redundant/external ones were dropped.

Until the cut, [m4](/quest/m4/README.md) and [m5](/quest/m5/README.md)
require the [release cut](/quest/m3/release/cut.md) (decided 2026-11-13,
maintainer), so spare capacity goes to m0 to m3
instead of to later features. m3 stays open, since it holds the cut and its
reviews. The cut's PR lifts m4's gate and moves m5's to the next cycle's
wire cut, below.

No quest requires one in a later milestone (decided 2026-11-13, maintainer):
a blocker ranks by its own milestone, so the earlier quest would stall behind
the later milestone's work. Additive work that builds on m2 joins m3, which
ranks right after m2, rather than m1.

A change is breaking when it is an API break or a wire change, as
[Releases](/RELEASE.md#releases) defines them. The test is whether it would need a breaking
release if it landed right after the cut, so a change to lite-07-wip or to a
`main`-only API the cut would publish counts. A non-urgent fix that breaks
joins m2; when a non-breaking fix exists, the fix joins m1 and the break m2;
until the `0.22.0` cut, the fix joins m3 unless the cut or lite-07
finalization needs it, which puts it in m2.
An epic joins the milestone its outcome needs, children included.

Decided 2026-11-05 (maintainer, milestone re-cut): milestones split by kind of
change for the current cycle and by priority after it. This replaces m1 as
"everything before the cut, breaking or not" (2026-10-20). Grouping by kind
could bury urgent breaking work behind additive work, so urgency still wins:
urgent work of either kind joins m0.

After the next cut, each release cycle runs in three phases by kind of
change (decided 2026-11-09, maintainer): additive work, then API breaks, then
wire changes, as [Releases](/RELEASE.md#releases) describes. `main` is
cut on demand in the first two phases, so their cuts are no longer gated on a
milestone; the wire phase is short and its cut ends the cycle. A change to an
unpublished `-wip` wire version is wire work.

The next cut performs a re-split, and each cut that ends a cycle shifts the
milestones (decided 2026-10-19, rule replaced 2026-11-05 and 2026-11-09). The
[release cut](/quest/m3/release/cut.md) does both:

- The next cut moves remaining release-execution quests to a release epic in
  m0, where the on-demand additive and API cuts recur, and sorts m3's
  other quests by kind: additive into m1, API breaks into m2, wire changes
  stay in m3. m4 keeps its quests as later work of higher priority, m5 starts
  empty as later work of lower priority, and today's m5 moves to m6, the
  deferred bucket. m1 is then additive work, m2 API breaks, and m3 wire
  changes.
- Each later cut that ends a cycle moves remaining release-execution quests
  to m0, sorts m4's quests by kind into m1, m2, and m3, moves m5's to m4, and
  leaves m5 empty and m6 the deferred bucket.

Ordering holds the phases: m1 work ranks first, and while `phase` in
`version.json` is `additive`, Nightly and the cut refuse a public API break on
`main`. Wire work is held by planning alone: it waits in m3 until the
maintainer opens the wire phase. The cut gate is re-created in m3, which the
cycle's last cut waits on. That wire cut lives in m4, the higher-priority
later-work milestone: it requires m2 and m3, so it can't sit in an earlier
milestone or in the m3 it waits on (decided 2026-11-14, under /quest-afk,
for the maintainer's review). m5 and m6 require it, so each cycle
spare capacity goes to m0 to m4, and m5's quests start once
the shift brings them into m4. The hygiene audits recur behind the cut in the
higher-priority later-work milestone (m3 until the next cut, m4 after it), so
the shift brings each into m1.

The [release execution](/quest/m3/release/README.md) starts once m2 is done.
It publishes artifacts before downstream adoption and rollout can complete.
Critical fixes continue to reach the existing release through backports.

Test flakes join m0 without asking (decided 2026-11-05, maintainer): a test
that fails intermittently in CI, Nightly, or the merge queue becomes a child
of m0's open `test-flakes-N` epic, or of a fresh one carrying the last epic's
rules from git history when none is open. Skills that plan
follow-ups plan a reported flake there without asking whether or where; the
planning PR's review is the check.
