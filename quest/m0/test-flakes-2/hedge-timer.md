# [S] No WebTransport hedge timer outlives its test

## Goal

`hedgeWebTransport`'s 500 ms hedge timer (`js/net/src/connection/connect.ts`)
never fires after the test that armed it, so the `bun test` host-clock guard
never fails an unrelated later test in `js/net/src/connection/connect.test.ts`.

## Plan

Planned 2026-11-07 (maintainer, /quest-complete), a follow-up of
[moq-dev/moq#8163](https://github.com/moq-dev/moq/pull/8163), whose CI run
[42402778685](https://github.com/moq-dev/moq/actions/runs/42402778685/job/128493969468)
failed `test js` this way on a PR that doesn't touch `js/net`. The failing
test varies with order, so none is quarantined.

The hedge races `timeout(WEBTRANSPORT_HEDGE, first)` against the first
dial. Find which test leaves that race pending at its end, and whether
`timeout` disarms its timer when the dial settles or is cancelled. Fix the
cause: a hedge whose dial is cancelled or abandoned must disarm its timer,
and a test that dials must settle or cancel its connect before it ends, or
run on a `Manual` clock. Prove it with a test that ends while the hedge is
pending and asserts no timer stays armed.

Public API: none. Wire: none.
