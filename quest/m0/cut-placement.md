# [M] Place each cycle's cuts so no milestone waits on a later one

## Goal

The [release cut](/quest/m3/release/cut.md)'s steps re-create each later cut
in a place where no quest requires one in a later milestone, and re-create
the gate that holds later work until that cut while the lab,
[m9](/quest/m9/README.md), stays open. The current cut follows them when it
re-creates its successor.

## Plan

Planned 2026-11-13 (maintainer, /quest-plan, on
[moq-dev/moq#8637](https://github.com/moq-dev/moq/pull/8637)): the same day
settled that no quest requires one in a later milestone, and that
[m4](/quest/m4/README.md) and [m5](/quest/m5/README.md) require this cut
until it lands. The cut's steps predate both: they re-create later cuts in
m0's release epic, and the wire cut that ends a cycle requires m2 and m3.

The constraints a placement must meet, all enforced by `quest check` once
moq-dev/quest's rule ships:

- A cut lives in or after the last milestone it waits on.
- A cut can't require the milestone that holds it, or any epic above it.
- A milestone gated on a cut can't hold that cut, or the gate is a cycle.

Settle with the maintainer where each phase's cut lives and which milestones
its gate covers, then rewrite the cut's steps and the root
[questline](/quest/README.md)'s cycle description to match.

## Related

- [Release cut](/quest/m3/release/cut.md) - holds the steps this rewrites
