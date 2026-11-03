# [S] JS follow takes FollowOptions

## Goal

In `@moq/net`, `Origin.Consumer.follow` (and the `Producer` and `Table`
passthroughs) takes its own `FollowOptions`, and `request` keeps
`RequestOptions`. Each options type documents only what its method does.
Every caller in `js/` compiles against the split, and the API report is
regenerated.

## Plan

Planned 2026-10-31 (`/quest-plan`, maintainer), a follow-up from
[JS follow returns a Following](https://github.com/moq-dev/moq/pull/7476),
which split the return types but kept one `RequestOptions` for both
methods. Keep the shared fields' names. Drop a field from `FollowOptions`
only if `follow` ignores it today, and say which in the PR. If the two
types still end up identical, a shared base with two aliases is fine; the
point is that each method's docs describe one behavior.

Public API: breaking for TypeScript type annotations that name
`RequestOptions` for a `follow` call. Wire: none.
