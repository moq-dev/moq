# [M] A browser consumer tells a requested end from a fault without reading message text

## Goal

Cutting a publisher off, or pointing `<moq-watch>` at another broadcast,
produces a classified end in `@moq/hang` and `@moq/watch` rather than a
`console.error` a caller has to pattern-match. A task whose stream was
cancelled has not failed and never reaches a catch-all logger. The browser
harness then fails on any unclassified error during a transition instead of
tolerating every error the page reports.

## Plan

Branch from dev, which already has what main lacks: `js/net` on dev carries
`StreamError` and `SessionError` with code registries, and `StreamCode.Cancel`
is the routine unsubscribe. On main only `RemoteError` exists, with code `0`
whenever the transport dropped the stream without one, so the classification
cannot be built there.

Today every path is the same path. A subscription aborted by a name change
or a departing publisher rethrows out of the `js/hang` group reader, lands in
`Effect.spawn`'s catch-all in `js/signals`, and prints `spawn error` plus
whatever the session constructed. `js/watch/src/video/decoder.ts` logs a bare
`DOMException` with no prefix and carries `// TODO bubble up error`. The
consequence is a test that cannot be strict: `test/smoke/clients/js/media.ts`
drives unsubscribe/rejoin and publisher-stop/republish, both of which abort
subscriptions on purpose, so `waitFrozen(.., tolerateErrors)` drains every
page error for the duration of the transition and a genuine decoder or
transport fault reads exactly like the abort the case asked for.

- Decide what `@moq/hang` and `@moq/watch` expose when a track, broadcast,
  or session ends by request: the dev `StreamError` with `Cancel` reaching
  the consumer as a clean end, and everything else as a fault. Whether hang
  and watch re-export the net types or wrap them is the open question; pick
  the shape that mirrors `rs/moq-net`'s `Error::Cancel` plus `finish` versus
  `abort`.
- Stop `Effect.spawn` being the reporting path for an expected end: a
  cancelled task resolves, it does not throw into the catch-all.
- Give the video decoder's `error` callback the same treatment, so the one
  unprefixed `console.error` in the player stops being unclassifiable. The
  truncated group a departing publisher leaves behind reaches the decoder as
  the same `DOMException` a broken decoder would, so the classification has
  to come from the stream's end, not the exception.
- Tighten `media.ts`: tolerate the classified end, fail immediately on
  anything else, and delete the blanket drain in `harness.ts`.

## Related

- [#2318](/quest/m1/2318-js-net-remaining-capability-gaps-vs-rs-moq-net-setup-role.md) - the other js/net gaps against the Rust model
- [Media QA on other engines](/quest/m2/browser-media-qa-engines.md) - the same harness, another axis
- [Failure artifacts](/quest/m0/qa-failure-artifacts.md) - shared trace and sample output
