# [XS] The open-GOP browser fixture fails loudly

## Goal

The open-GOP browser test (`test/interop/clients/js/open-gop.browser.ts`)
exercises current `@moq/watch` code: its broadcast stub satisfies the stats
dependencies the player now reads, and an exception inside the fixture's
effect fails the test directly instead of being swallowed.

## Plan

Planned 2026-11-05 (maintainer, /quest-plan of the follow-ups from the
2026-11-05 /quest-complete run), from
[moq-dev/moq#7716](https://github.com/moq-dev/moq/pull/7716). Milestone: ✅ m0.

The stub lives in `test/interop/clients/js/open-gop/main.ts`. Prefer building
the stub from the real types so the next dependency change fails to compile
rather than at runtime.

Public API: none. Wire: none.
