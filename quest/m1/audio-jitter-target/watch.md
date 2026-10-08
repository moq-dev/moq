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

`playout.test.ts` measures synthetic traces through a transport subscription
into `Container.Consumer` on a stubbed clock and sizes both rings from the
target that settles. In `"auto"` the audio subscription asks for at least the
estimator's 2 s ceiling, as native does; a subscription cut to the target hid
every frame later than it. `demo/web` sets no delay and `js/watch` stores none,
so a fresh tile runs auto.

The harness's `js/watch/src/audio/replay.ts` plays the recorded traces in
`test/audio-quality/traces/` (the #3477 traces are gone) through
`Container.Consumer` and both rings on a simulated clock, and resolves `"auto"`
from the same estimator through a real `Sync`, resizing the ring as it moves.
Its exact budgets were re-measured when `main` merged in: underruns fell to at
most 2 a minute on every trace, but a ring that runs dry now re-stalls and
refills to the target, so `stalled_share` and `silence_share` rose (up to 3% on
`relay-bbb` and `relay-mic`), and `relay-mic` converges from a 420 ms p95 over
about 22 s with skip-aheads on the way down.

- Decide whether those re-stall refills are what the goal wants, using the
  replay rows as the measurement. Tighten the replay budgets with any fix.
  Re-record the browser rows' `test/audio-quality/budgets.json` from the
  nightly runner's first runs on `main` before tightening them, since they were
  recorded locally (#4426) and nightly only runs `main`'s code.
- The replay sizes every frame as the trace's median spacing, which a trace
  missing most of its frames defeats; record the codec's frame duration in the
  trace and replay that instead.
- Manual run against the public relay on Chrome and Safari, the two rows the
  issue measured, with a real microphone and 40 ms or more of added RTT. Watch
  the stats panel's audio underrun counter and the latency tab's auto readout,
  on both the isolated and the postMessage ring paths. Re-record the
  `relay-mic` trace in the same run (`just test audio-quality-record`); the
  checked-in one used Chromium's fake capture device. Measure the publisher's
  audio encoder input-to-output lag in the same run using the reporter's
  instrumented harness; #3518 fixed the known cause, so the 7.35 s lag and the
  88 to 275 ms/s drift the issue reported stand unconfirmed. If drift survives,
  the suspects are `writeFrame` opening a group per audio frame under
  WebTransport stream credit and the main-thread task queue delivering encoder
  output. The audio encoder's `groupDuration` (#4910, default 0: a group per
  frame) tests the first: A/B it against a group of 100 ms or so in the same
  run. Turn that into its own quest rather than fixing it here.

Decided 2026-10-08: moves to m1 with the rest of the line. The estimator
defaults on and only this manual proof is left, so it no longer gates a
release.

## Related

- [A/V clock](/quest/m1/av-clock.md) - reshapes `SyncInput` around the per-track target this line produces
