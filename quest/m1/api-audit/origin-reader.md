# [S] @moq/net names the origin read side

## Goal

@moq/net exports a named read-side origin interface, `Origin.Reader`, that
`Origin.Table` extends, so readers such as @moq/watch accept that one name
instead of repeating a `Pick<Origin.Consumer, ...>` list.

## Plan

[moq-dev/moq#7251](https://github.com/moq-dev/moq/pull/7251) makes @moq/watch
accept a structural read-side origin and leaves the named interface as a
follow-up. Name it before the API cut, and move existing `Pick` lists to it.
If #7251 has not merged, `Player` and `Broadcast` still take `Origin.Table`:
widen their origin inputs to `Origin.Reader` here.

Decided 2026-10-30 (maintainer, /quest-plan of the 2026-10-30 /quest-complete follow-ups): ✅ new m1 api-audit quest / drop.

Public API: @moq/net adds `Origin.Reader`; @moq/watch's origin inputs accept
it. Both are additive, so nothing migrates. Wire: none.
