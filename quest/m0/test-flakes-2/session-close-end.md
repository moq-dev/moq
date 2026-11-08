# [S] js/net's lite-06 session-close test keeps the end under load

## Goal

`js/net/src/lite/datagram-tracks.test.ts`'s "moq-lite-06: a session close right
after a datagram track's end keeps the end" passes under load and runs again.

## Plan

Planned 2026-11-08 (maintainer). The lite-06 case failed in the merge queue,
dropping [moq-dev/moq#8280](https://github.com/moq-dev/moq/pull/8280)
([job](https://github.com/moq-dev/moq/actions/runs/42489349640/job/128758087035)):
`reader.recv()` rejected with `Session: remote error: 0` from `closeError`
(`js/net/src/error.ts`) instead of returning the end. The lite-05 and lite-07
cases still run; the lite-06 case is quarantined with `test.skip`.

Suspects: the test drains 10,000 microtasks and assumes the end reached the
reader by then. Find which lite-06 step (SUBSCRIBE_FINISH, the FIN, or the
track's host-task wait for its datagrams) can still be pending when
`client.abort()` lands, and whether the library or the test is wrong: the
comment above the test says a close after the end must keep the end. Wait on
that event, never a longer drain, timeout, or retry. Reproduce with
`bun test --rerun-each 200` under a loaded machine first. Then remove the skip
and delete this quest.

Investigated 2026-11-08 (no repro): on `main` and on the failing queue's base
`12e41a369e`, with Bun 1.4.2 as in CI, 24 parallel copies of
`--rerun-each 300` of the lite-06 case (7,200 runs each), 640 loaded runs of
the whole file, and 12 parallel full `js/net` suites all passed. Instrumenting
`@moq/clock`'s host `task()` showed the case runs in one host task: only the
two `AuthSession` decision yields and the subscriber's `#datagramsRouted`
yield are pending at `client.abort()`, and the publisher's post-update
`yieldNow()` is not. Forcing a host task before `reader.info()`, the priority
wait, `finish()`, or the abort still passed. In the CI log the lite-06 stream
track failed (`subscribe error`, no `subscribe close`), so its responses or
`#settleTail` were unsettled, not the datagram routing. Since load cannot
reorder microtasks, the remaining suspects read real time: `Budget` (stubbed
only after `connect` resolves), and `now()` in `group.ts`/`cache.ts`.

Public API: none. Wire: none.
