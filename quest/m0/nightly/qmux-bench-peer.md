# [XS] The qmux startup benchmark's fake peer sends transport parameters

## Goal

Nightly's Audio quality job passes its "WebSocket fallback startup benchmark"
step: `just js qmux-bench` completes instead of failing with
`error: evaluate: Error: handshake timeout`.

## Plan

Planned 2026-11-09 (maintainer, /quest-complete from the triage of Nightly
runs [42377867523](https://github.com/moq-dev/moq/actions/runs/42377867523)
and [42487375545](https://github.com/moq-dev/moq/actions/runs/42487375545)).
Failed both nights.

The Bun server in `js/net/bench/qmux.ts` never sends QMux transport
parameters, and the JS session now waits for them before it opens.
[moq-dev/moq#8290](https://github.com/moq-dev/moq/pull/8290) fixes the same
fake-peer handshake in `js/qmux/bench/datagrams.ts`, but Nightly runs the
`js/net` copy. Make the fake peer send them the way the real server does,
ideally through the same helper as `datagrams.ts` so the two cannot drift.

Public API: none. Wire: none.
