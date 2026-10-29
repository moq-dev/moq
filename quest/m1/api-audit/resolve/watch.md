# [S] The watch player's follow prop

## Goal

`@moq/watch`'s `Broadcast` names its resolution choice `follow` (default
true), mirroring `@moq/net`'s `follow` and `request`, instead of
`announced`. Passing `announced` throws a rename error, as `reload` already
does.

## Plan

Decided 2026-10-29 (maintainer), a follow-up of
[moq-dev/moq#7173](https://github.com/moq-dev/moq/pull/7173), which renamed
the `@moq/net` side and left the player surface out of scope. One vocabulary
across `@moq/net` and the player, so the prop reads as the method it picks.

Update the watch docs and README, and any in-tree caller that passes the prop.

Public API: breaking in `@moq/watch`, with an Impact migration note. Wire:
none.
