# [XS] The open-GOP recovery fixture provides the broadcast name

## Goal

The scheduled Interop run's "Harness regressions" step passes the
`open-gop.browser.ts` case "software video recovery trims orphan pictures
after join and later gap". The fixture's fake broadcast provides everything
`@moq/watch`'s decoder reads. From now on, an exception thrown inside a
fixture's effect fails the test immediately with that error, instead of
surfacing as a 60-second timeout.

## Plan

Decided 2026-11-03 (maintainer, /quest-plan of the follow-ups from
[moq-dev/moq#7715](https://github.com/moq-dev/moq/pull/7715) and
[moq-dev/moq#7716](https://github.com/moq-dev/moq/pull/7716)): milestone m0,
a child of the Interop epic, since it breaks the scheduled run.

- Cause: [moq-dev/moq#7222](https://github.com/moq-dev/moq/pull/7222)
  (`2738588cfc`) made `js/watch/src/video/decoder.ts` read
  `broadcast.in.name.peek()` for stats. The fake broadcast in
  `test/interop/clients/js/open-gop/main.ts` provides only `out: { catalog,
  active }` and `relativeBroadcast`, so the read throws inside an effect.
  The scheduled run
  [41668668824](https://github.com/moq-dev/moq/actions/runs/41668668824)
  fails with `TypeError: Cannot read properties of undefined (reading
  'name')` at `window.runRecovery`.
- Add `in: { name }` to the stub, typed against the real broadcast's shape
  where the fixture allows it, so the next field the decoder reads fails
  type-checking instead of at run time.
- Fail fast: route page errors and effect exceptions to a test failure.
  [moq-dev/moq#7716](https://github.com/moq-dev/moq/pull/7716) adds
  page-error collection to `harness.browser.ts`. Reuse it if it has landed,
  or add the same collection here and let #7716 merge onto it.

Public API: none. Wire: none.
