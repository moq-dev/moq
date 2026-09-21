# [M] @moq/net answers the questions every app asks

## Goal

An application on `@moq/net` iterates announcements, reads the live set of
broadcasts as a getter, serves on demand through a pooled `Connection`,
and refreshes its credential before a redial, without writing the loops
moq.pro's dashboard wrote (`app/src/lib/live.svelte.ts`).

## Plan

- `Origin.Table.broadcasts(scope): Getter<ReadonlyMap<Path.Valid, Route>>`
  for the "what is live" case every UI wants, beside the async iterator
  landed in [#3770](https://github.com/moq-dev/moq/pull/3770).
- `Origin.Table.dynamic()`; `Connection.origin` is typed `Table`, which has
  `createBroadcast` and `request` but not `dynamic` though the object is a
  `Producer`.
- `ConnectionProps.url` accepts `() => Promise<URL>` called before each
  dial (or a `credential` getter merged into the query), so a stale `?jwt`
  is re-minted by the library instead of a status watcher.
- `share` is inferred: any of `webtransport`, `websocket`, `discovery`,
  `delay`, `publish`, or `consume` already implies a private loop, so the
  six refusals become a default and `share: true` stays an explicit request.

Public API: additive on @moq/net. Wire: none.

## Required

- [Merge dev](/quest/dev/merge-dev.md) - starts on main
