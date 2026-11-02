# [XS] Browser tests expect current behavior

## Goal

The Video workflow's renderer test and the Interop harness's element
version tests assert the behavior the elements ship.

## Plan

- `test/video/renderer.browser.ts` expects an invalid `url` and `volume`
  attribute to reset to their defaults with no page errors. The elements now
  keep the last valid value, report the invalid attribute as a page error,
  and suspend media until it is fixed (`js/watch/MIGRATE.md`). Assert that
  contract: the last valid value is kept, each invalid attribute reports
  once, and media stays suspended until a valid value returns. It fails
  every Video run on main.
- `test/interop/clients/js/publish-ui.browser.ts` (the "version lists keep
  their defaults" and "version lists refuse unsupported" tests) uses
  `moq-transport-14`, which
  [moq-dev/moq#6753](https://github.com/moq-dev/moq/pull/6753) removed, so
  the setter throws `unsupported version`. Use a supported draft where the
  test means a valid version and keep an unsupported one only where it
  tests the refusal.

Public API: none. Wire: none.
