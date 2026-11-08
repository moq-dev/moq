# [XS] The http:// fingerprint tests survive a slow fetch

## Goal

`js/net/src/connection/connect.test.ts`'s http:// fingerprint tests pass
under load: a fingerprint fetch slower than `hedgeWebTransport`'s 500 ms
hedge no longer fires a host-clock timer that the `bun test` guard fails.

## Plan

Planned 2026-11-07 (maintainer, /quest-complete), a follow-up of
[moq-dev/moq#8163](https://github.com/moq-dev/moq/pull/8163), whose CI run
[42402778685](https://github.com/moq-dev/moq/actions/runs/42402778685/job/128493969468)
failed `test js` on a PR that doesn't touch `js/net`. The failing test,
"an http:// fingerprint longer than a SHA-256 is refused without reading
the rest" (`connect.test.ts:176` at the time), took 894 ms: its connect
arms the hedge timer before `connectWebTransport` fetches the fingerprint
from a real `Bun.serve` server, the fetch outlasted 500 ms, and the timer
fired on the host clock inside that same test. `timeout` already disarms
when its dial settles (`js/clock/src/timer.ts`), so no timer outlives its
test.

The three http:// fingerprint tests at `:147`, `:176`, and `:208` talk to a
real HTTP server but lack the `hostTime(); // a real HTTP server` opt-out
their sibling at `:230` carries, which the mock-clock epic's rules allow for
real I/O. Decide in the PR between adding that opt-out and arming the hedge
only after the fingerprint resolves, so a slow fetch can't start a second
dial. Prove it by delaying the fingerprint response past the hedge.

Public API: none. Wire: none.
