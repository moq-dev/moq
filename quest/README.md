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

Milestones shift after each cut (decided 2026-10-19, rule replaced
2026-11-05). The [release cut](/quest/m3/release/cut.md) recurs and performs
the shift: remaining release-execution quests move to m0, m3's quests split
by kind into m1 and m2, m4's move to m3, m4 starts empty, and m5 stays the
deferred bucket. The cut PR also re-creates the cut gate in m2. The hygiene
audits recur once per major release in m3, behind the cut, so the shift
brings each into m1.

The [release execution](/quest/m3/release/README.md) starts once m2 is done.
It publishes artifacts before downstream adoption and rollout can complete.
Critical fixes continue to reach the existing release through backports.

Test flakes join m0 without asking (decided 2026-11-05, maintainer): a test
that fails intermittently in CI, Nightly, or the merge queue becomes a child
of m0's [test flakes](/quest/m0/test-flakes-2/README.md) epic. Skills that plan
follow-ups plan a reported flake there without asking whether or where; the
planning PR's review is the check.
