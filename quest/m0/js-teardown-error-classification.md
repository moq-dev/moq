# [M] Tell a teardown abort apart from a fault in the browser

## Goal

A browser consumer can tell "this subscription ended because you ended it" from
"something broke" without reading message text. Cutting a publisher off, or
pointing `<moq-watch>` at another broadcast, produces a classified end rather
than a `console.error` a caller has to pattern-match.

## Plan

Today every path is the same path. A subscription aborted by a name change or a
departing publisher rethrows out of `js/hang`'s group reader, lands in
`Effect.spawn`'s catch-all, and prints `spawn error` plus whatever `Error` the
session happened to construct: `cancel`, `track is closed`,
`broadcast is closed: null`, `remote error: 0`. `js/watch/src/video/decoder.ts`
logs a bare `DOMException` with no prefix at all and carries a
`// TODO bubble up error`. Only `RemoteError` has a machine-checkable shape, and
its code is `0` whenever the transport dropped the stream without one.

The consequence is a test that cannot be strict. `test/smoke/clients/js/media.ts`
drives unsubscribe/rejoin and publisher-stop/republish, both of which abort
subscriptions on purpose, so its waits tolerate every error the page reports for
the duration of the transition. Everything is still echoed to the log, and the
measurement window after each transition is fatal on any error, but during the
transition a genuine decoder or transport fault reads exactly like the abort the
case asked for. A message allowlist would not fix that: the truncated group a
publisher leaves behind reaches the decoder as the same `DOMException` a broken
decoder would.

- Decide what a consumer should see when a track, broadcast, or session ends by
  request rather than by fault. A distinct error type carrying that reason is the
  obvious shape; whether it belongs in `@moq/net` alone or is mirrored in
  `@moq/hang` and `@moq/watch` is the open question.
- Stop `Effect.spawn` being the reporting path for an expected end. A task whose
  stream was cancelled has not failed and should not reach a catch-all logger.
- Give the video decoder's `error` callback the same treatment as the rest, so
  the one unprefixed `console.error` in the player stops being unclassifiable.
- Mirror whatever lands in `rs/moq-net`, which already separates a close from an
  abort, so the two languages agree on what a caller is told.
- Then tighten `media.ts`: tolerate the classified end and fail immediately on
  anything else, and delete the blanket drain.

## Related

- [Media QA on other engines](/quest/m0/browser-media-qa-engines.md) - the same harness, another axis
- [Failure artifacts](/quest/m0/qa-failure-artifacts.md) - shared trace and sample output
