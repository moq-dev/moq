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
would need one if it landed right after the cut, plus the
cut gate the [release cut](/quest/m3/release/cut.md) re-creates there. The cut waits on m2 alone. m3 and m4
are later work split by priority: m3 holds what a named consumer waits on,
release execution, and bug fixes in shipped code; m4 holds tests, tooling,
benchmarks, optimizations, and features nobody waits on yet. m5 holds work
gated on the outside world, or speculative work with no named consumer.

[m9](/quest/m9/README.md) is the lab (decided 2026-11-12, maintainer,
/quest-audit; renamed from `lab` 2026-11-13): experiments and measurements
that touch no published API, wire format, or CLI surface. They start any
time, merge whenever, never gate a cut, and stay in place when the
milestones shift. `quest` orders milestones by name, so `lab` ranked ahead
of m0; as m9 it ranks last.

Until the cut, [m4](/quest/m4/README.md) and [m5](/quest/m5/README.md)
require the [release cut](/quest/m3/release/cut.md) (decided 2026-11-13,
maintainer), so spare capacity goes to m0 to m3 and then to m9's lab work
instead of to later features. m3 stays open, since it holds the cut and its
reviews. The cut's PR deletes the cut quest, which lifts the gate.

No quest requires one in a later milestone (decided 2026-11-13, maintainer):
a blocker ranks by its own milestone, so the earlier quest would stall behind
the later milestone's work. Additive work that builds on m2 joins m3, which
ranks right after m2, rather than m1.

A change is breaking when a published consumer or a deployed peer must adapt
to it: the public Rust, JS, or bindings API, a frozen wire version, the hang
catalog JSON, a stored or exported container format, or CLI flags, config
keys, and environment variables. The test is whether it would need a breaking
release if it landed right after the cut, so a change to lite-07-wip or to a
`main`-only API the cut would publish counts. A non-urgent fix that breaks
joins m2; when a non-breaking fix exists, the fix joins m1 and the break m2.
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
milestone; the wire phase is short and its cut ends the cycle. An API break
is what a published consumer adapts to when it rebuilds: the public Rust, JS,
or bindings API, CLI flags, config keys, and environment variables. A wire
change is what a deployed peer adapts to without rebuilding: a frozen wire
version, the hang catalog JSON, or a stored or exported container format,
since a deployed peer parses those too. A change to an unpublished `-wip`
wire version is wire work.

The next cut performs a re-split, and each cut that ends a cycle shifts the
milestones (decided 2026-10-19, rule replaced 2026-11-05 and 2026-11-09). The
[release cut](/quest/m3/release/cut.md) does both:

- The next cut moves remaining release-execution quests to a release epic in
  m0, where each cut re-creates its successor, and sorts m3's
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
cycle's last cut waits on. The hygiene audits recur behind the cut in the
higher-priority later-work milestone (m3 until the next cut, m4 after it), so
the shift brings each into m1.

The [release execution](/quest/m3/release/README.md) starts once m2 is done.
It publishes artifacts before downstream adoption and rollout can complete.
Critical fixes continue to reach the existing release through backports.

Test flakes join m0 without asking (decided 2026-11-05, maintainer): a test
that fails intermittently in CI, Nightly, or the merge queue becomes a child
of m0's [test flakes](/quest/m0/test-flakes-2/README.md) epic. Skills that plan
follow-ups plan a reported flake there without asking whether or where; the
planning PR's review is the check.
