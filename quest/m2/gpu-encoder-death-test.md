# [S] A browser test covers GPU encoder death and a broadcast rename

## Goal

A real-browser test makes `@moq/publish`'s video encoder fail mid-stream (as
when the GPU process dies), then renames the network broadcast, and asserts
the failed rendition stays out of the catalog while failed, then comes back
under a fresh track name on the new broadcast with no carried-over failure.

## Plan

Decided 2026-10-31 (user, /quest-plan of the follow-ups from the 2026-10-31
/quest-complete run), from
[moq-dev/moq#7371](https://github.com/moq-dev/moq/pull/7371): m2, as
recommended. Start after [moq-dev/moq#7371](https://github.com/moq-dev/moq/pull/7371) (open) lands; it moves encoder failure into
each publication's slot, so a new network broadcast starts clean, and covers
that only with unit tests.

- Put it in `test/interop/clients/js/` beside `publish-ui.browser.ts`.
  Inject the failure through the encoder (closing the `VideoEncoder` or a
  test hook on its error path) rather than killing a real GPU process.
- Check the catalog and track names from a subscriber, not internal state.

Public API: none. Wire: none.

## Related

- [Publish publication](/quest/m2/publish-publication.md) - [moq-dev/moq#7371](https://github.com/moq-dev/moq/pull/7371) lands the slot this tests
