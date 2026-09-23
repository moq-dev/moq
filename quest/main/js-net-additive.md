# [M] @moq/net answers the questions every app asks

## Goal

An application on `@moq/net` iterates announcements, reads the live set of
broadcasts as a getter, serves on demand through a pooled `Connection`,
and refreshes its credential before a redial, without writing the loops
moq.pro's dashboard wrote (`app/src/lib/live.svelte.ts`).

## Plan

- `Origin.Table.broadcasts(scope): Getter<ReadonlyMap<Path.Valid, Route>>`
  for the "what is live" case every UI wants, beside the async iterator
  landed in [#3770](https://github.com/moq-dev/moq/pull/3770). Unscoped readers
  share one snapshot; distinct scoped readers filter separately. A nightly
  two-axis benchmark measures both cases.
- `Origin.Table.dynamic()`; `Connection.origin` is typed `Table`, which has
  `createBroadcast` and `request` but not `dynamic` though the object is a
  `Producer`.
- Credential refresh before each redial is pending the maintainer's API choice:
  either `ConnectionProps.url: () => Promise<URL>` or a `credential` getter
  merged into `?jwt`. On UNAUTHORIZED, resolve once and retry only if the
  resulting URL differs from the rejected URL.
- `share` is inferred: any of `webtransport`, `websocket`, `discovery`,
  `delay`, `publish`, or `consume` already implies a private loop, so the
  six refusals become a default and `share: true` stays an explicit request.

Public API: additive on @moq/net. Wire: none.
