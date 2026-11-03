# [XS] Stats name the route instance's epoch with a type

## Goal

`moq_stats::GroupFrame::epoch` carries a dedicated type rather than a bare
integer, and the routing contract tests stop labelling the instance's value
`epoch` where they mean the instance, so the public stats API matches the
instance/epoch naming before the cut.

## Plan

Planned 2026-11-02 (maintainer, follow-up planning after the 2026-11-02 /quest-complete run), leftovers from [moq-dev/moq#7489](https://github.com/moq-dev/moq/pull/7489)
(routes carry an instance and a per-instance epoch).

Public API: the `GroupFrame` field's type changes.
