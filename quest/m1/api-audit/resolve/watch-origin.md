# [S] The player takes any read-side origin

## Goal

`@moq/watch`'s `Player` and `Broadcast` accept a read-only `Origin.Consumer`
as well as an `Origin.Table`, so `new Player({ origin: producer.consume() })`
type-checks.

## Plan

Decided 2026-10-27 (maintainer): both take a structural read-side origin type
(`closed`, `discovery`, `request`, `follow`, `announced`), not
`Origin.Table`. Today the call above fails to type-check because `Table` also
requires `createBroadcast` and `requests`. Retype every `Moq.Origin.Table`
input in `js/watch/src/player.ts` and `broadcast.ts`. Do not add an adapter
or a parallel player class.

The `request(path, { announced })` calls already map onto `follow` and
`request` ([moq-dev/moq#7173](https://github.com/moq-dev/moq/pull/7173)).

Public API: widens `@moq/watch`'s accepted input; not breaking.
Wire: none.
