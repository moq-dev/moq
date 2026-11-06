# [M] A reader on a failed copy continues on the held re-offer

## Goal

A live reader already handed a copy of a group that then fails keeps the
group: its `group::Consumer` continues on the successor copy the held
re-offer carries in the same track, at the same frame, instead of surfacing
the failure. Rust and `js/net` behave the same.

## Plan

Planned 2026-11-05 (maintainer, /quest-plan of the follow-ups from the
2026-11-05 /quest-complete run), from #7670. Milestone: ✅ m1.

`with_recover` already continues a reader across a front's routes; reuse it
or its mechanism for the same-track successor rather than adding a parallel
path. A successor that diverges from what the reader already consumed fails
the reader, as a route switch does. Test on mock time: a reader mid-group on a
copy that fails continues on the re-offer with no duplicate or missing frame.

Decided 2026-11-05 (maintainer, /quest-audit): ✅ the wait on
[moq-dev/moq#7670](https://github.com/moq-dev/moq/pull/7670), Drop set's own
PR, becomes a `Required` link to [Drop set](/quest/m1/drop-set.md), since
#7670 adds the held re-offer (its decision 24) this builds on.

Public API: none expected. Wire: none.

## Required

- [Drop set](/quest/m1/drop-set.md) - adds the held re-offer this builds on
