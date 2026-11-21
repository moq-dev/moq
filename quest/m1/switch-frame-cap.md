# [M] A route switch caps the old subscription at a frame

## Goal

A lite-07 Range can end at a Position (a group's start and a frame), so a
route switch caps the old subscription at the exact frame where the
replacement starts. The old publisher sends nothing of the boundary group
past that frame, beyond what was already in flight. Rust, `@moq/net`, and the
moq-lite draft change together.

## Plan

Decided 2026-11-20 (maintainer, /quest-plan auditing the subscription rules
before the lite-07 cut): a switch caps the old subscription by group first
([switch-caps-old](/quest/m1/switch-caps-old.md)), and this quest adds the
frame cap on top. It isn't Required by
[the lite-07 cut](/quest/m2/lite07-finalize.md). It rides lite-07 if it has
landed when the cut is made, and otherwise moves to the next wip version.

- Today a Position is a `Start` only (draft, SUBSCRIBE's Anchor Type), and the
  Positions section says a subscription "never stops partway through" a
  Group. Allow Position as an `End` too, ending before that frame. Check how
  it reads beside "both ends include the Group they name".
- A publisher reaching a Position end stops the group's stream without a FIN
  (maintainer: a later SUBSCRIBE_UPDATE may widen the range to include the
  rest). The stream stays open until the range widens, the group expires, or
  the subscription ends. Account for the credit it holds, per moq-net's rule
  that no stream is finished and forgotten.
- The switch then ends the old copy's ranges at Position (G, N) instead of
  group G, and drops the step that stops G's stream once the replacement
  passes it.
- moq-transport has no frame end; its publisher serves the whole group, as
  today.
- Run `just test interop --all`.

Public API: the range end type gains a Position in Rust, JS, and the
bindings. Wire: lite-07-wip if before the cut, else the next wip version.

## Required

- [A route switch caps the old subscription at one boundary](/quest/m1/switch-caps-old.md) - the switch-time cap this narrows to a frame
