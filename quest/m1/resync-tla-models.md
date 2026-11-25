# [L] Resync drifted TLA+ models before 0.22

## Goal

The `verify-resume`, `verify-failover`, `verify-credit`, and `verify-warm` TLA+
models match the `main` implementation of the functions they cover, pass every
config, and record a sync commit at the PR's base, so 0.22 ships lite-07 code
its models still check.

## Plan

Found by the 2026-11-23 pre-0.22 test coverage audit (`890ea4e531`). Commits on
`main` touching each skill's covered files since its sync commit, counted
2026-11-25 with the `git log` line in each skill's Coverage section:

- `.agents/skills/verify-resume/SKILL.md`: sync `8c939bc9d0`, 72 commits.
- `.agents/skills/verify-failover/SKILL.md`: sync `46059266ec`, 75 commits.
- `.agents/skills/verify-credit/SKILL.md`: sync `e40a7a4340`, 17 commits.
- `.agents/skills/verify-warm/SKILL.md`: sync `1399add3c8`, 3 commits.

A commit touching a covered file need not change a covered function, so walk
each log against the skill's Coverage table, update the spec and configs for
each behavioral change (such as per-range `SUBSCRIBE_START`, `Max Age`,
time-span anchors, and the frame-level switch cutoff), run every config through
`quest gate just rs tla ...`, fix any real bug TLC finds at its root, and
advance each sync commit. Check `js/*/verify/` the same way.

Left out: `verify-subscribe`, which
[previous-delta](/quest/m2/lite07-subscribe/previous-delta.md) and
[per-range-policy](/quest/m2/lite07-subscribe/per-range-policy.md) rewrite for
lite-07, so resyncing it first would be thrown away; and `verify-reconnect`,
which is current. Open [moq-dev/moq#9529](https://github.com/moq-dev/moq/pull/9529)
edits `ResumeReader.tla` and its skill, so start `verify-resume` from its head
if it is still open.

Public API: none. Wire: none, unless TLC finds a lite-07 wire bug.
