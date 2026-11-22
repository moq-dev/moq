# [S] A snapshot consumer reports a schema refusal without callers matching on errors

## Goal

`@moq/room`'s `subscribeJson` stops matching `err instanceof z.core.$ZodError` to tell a value
that failed its schema (skip it, keep the subscription) from a real failure (end it), which breaks
the repository rule against matching on errors. `@moq/json`'s `Snapshot.Consumer` exposes that
distinction in its API instead, and every caller with a schema uses it.

## Plan

Found by the [js-media-fixes](https://github.com/moq-dev/moq/pull/9344) PR while it reworked
`js/room/src/metadata.ts`.

Recommended shape: a `Snapshot.Consumer` given a schema skips a value that fails it on its own,
like a gap, and counts or exposes the refusal (an `out.refused` signal holding the last issue)
so `latest()` and `next()` only ever reject on a real failure. Mirror the shape in
`rs/moq-json`'s snapshot consumer if it has the same split, and drop the `instanceof` branch from
`js/room/src/metadata.ts`.
