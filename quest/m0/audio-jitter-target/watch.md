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

- Replay the recorded traces from #3477 rather than synthetic ones of the same
  shape. They are on the reporter's fork (`fperex/moq`, branch
  `debug/rt-audio`) with the raw ndjson attached to release
  `rt-audio-traces-2026-09-06`. Trim a copy into the repository and replay it
  through both rings in `js/watch/src/audio/replay.test.ts`, which today sizes
  the rings from a p95 approximation rather than from the estimator.
- Manual run against the public relay on Chrome and Safari, the two rows the
  issue measured. Watch the stats panel's audio underrun counter and the
  latency tab's auto readout. Measure the publisher's audio encoder
  input-to-output lag in the same run using the reporter's instrumented harness;
  #3518 fixed the known cause, so the 7.35 s lag and the 88 to 275 ms/s drift
  the issue reported stand unconfirmed. If drift survives, the suspects are
  `writeFrame` opening a group per audio frame under WebTransport stream credit
  and the main-thread task queue delivering encoder output. Turn that into its
  own quest rather than fixing it here.
- `demo/web` no longer pins each tile to a fixed 100 ms delay, which is why a
  fresh session came up on the 100 ms chip. Confirm auto is what the demo now
  exercises.
- In auto mode the subscription's `maxAge` follows the target, so a group
  staler than the current target is dropped before `Container.Consumer`
  observes it, and the estimate cannot rise past what it already allows.
  Native gives auto a separate 2 s budget (`AUTO_MAX_AGE` in `moq play`).
  Decide from the run whether the browser needs the same.

## Related

- [Plan: A/V clock](/quest/m0/plan-av-clock.md) - reshapes `SyncInput` around the per-track target this line produces
