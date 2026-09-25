# [S] js/watch: prove the measured playout target in a real browser

## Goal

The "Real-time" preset plays clean audio on a LAN and against the public relay:
zero underruns after convergence and no skip-aheads in steady state, on both the
isolated and the postMessage ring paths, confirmed by a manual run on Chrome and
Safari with a real microphone and 40 ms or more of added RTT.

Boundaries: the estimator already conforms to `doc/concept/audio-jitter.md` and
passes the corpus in `js/hang/src/container/jitter.test.ts`. This quest measures
it, and fixes only what the measurement shows.

## Plan

`replay.test.ts` measures each trace through a transport subscription into
`Container.Consumer` on a stubbed clock and sizes both rings from the target
that settles, so a recorded trace drops in as an `Arrival[]`. In `"auto"` the
audio subscription asks for at least the estimator's 2 s ceiling, as native
does; a subscription cut to the target hid every frame later than it.
`demo/web` sets no delay and `js/watch` stores none, so a fresh tile runs auto.

- Replay the recorded traces from #3477 rather than synthetic ones of the same
  shape. The raw ndjson was attached to release `rt-audio-traces-2026-09-06` on
  the reporter's fork (`fperex/moq`), which no longer exists; the
  `debug/rt-audio` branch keeps only per-run summaries and event logs. Ask the
  reporter to re-attach them, or record fresh ones with the
  [browser harness](/quest/m1/audio-quality-harness/browser.md), then trim a
  copy into the repository and replay it in `js/watch/src/audio/replay.test.ts`.
- Manual run against the public relay on Chrome and Safari, the two rows the
  issue measured, with a real microphone and 40 ms or more of added RTT. Watch
  the stats panel's audio underrun counter and the latency tab's auto readout,
  on both the isolated and the postMessage ring paths. Measure the publisher's
  audio encoder input-to-output lag in the same run using the reporter's
  instrumented harness; #3518 fixed the known cause, so the 7.35 s lag and the
  88 to 275 ms/s drift the issue reported stand unconfirmed. If drift survives,
  the suspects are `writeFrame` opening a group per audio frame under
  WebTransport stream credit and the main-thread task queue delivering encoder
  output. Turn that into its own quest rather than fixing it here.

## Related

- [Plan: A/V clock](/quest/m0/plan-av-clock.md) - reshapes `SyncInput` around the per-track target this line produces
