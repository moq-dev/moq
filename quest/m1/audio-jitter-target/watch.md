# [S] js/watch: prove the measured playout target in a real browser

## Goal

The "Real-time" preset plays clean audio on a LAN and against the public relay:
zero underruns after convergence and no skip-aheads in steady state, confirmed
by a manual run on Chrome and Safari with a real microphone and 40 ms or more of added RTT.

Boundaries: the estimator already conforms to `doc/concept/audio-jitter.md` and
passes the corpus in `js/hang/src/container/jitter.test.ts`. This quest measures
it, and fixes only what the measurement shows.

## Plan

`playout.test.ts` measures synthetic traces through a transport subscription
into `Container.Consumer` on a stubbed clock and sizes the ring from the
target that settles. In `"auto"` the audio subscription asks for at least the
estimator's 2 s ceiling, as native does; a subscription cut to the target hid
every frame later than it. `demo/web` sets no delay and `js/watch` stores none,
so a fresh tile runs auto.

The harness's `js/watch/src/audio/replay.ts` replays the version 3 traces in
`test/audio-quality/traces/` through `Container.Consumer`, a real `Sync`, and
the ring on a simulated clock. Each arrival carries an encoded duration
from container metadata, the Opus TOC, or the codec's fixed sample count.
Replay never infers a frame's duration from timestamp spacing. The AAC-LC
fixtures retain their original arrivals with exact 1024-sample durations;
`relay-mic` was re-recorded with Chromium's fake capture device because the
old recording retained no payloads from which to recover Opus durations.

Re-measured on 2026-10-22: all four replay rows have zero underruns. The
fresh `relay-mic` has zero skips, stalls, or silence, a 60 ms p95 target,
and 3.5 s convergence, over a 2 ms minimum RTT. It is a new
fake-device recording, so that result proves neither an improvement on
the old path nor the manual goal. The AAC recordings still refill: worst
stalled share is 1.7% (`relay-bbb`), with `local-aac` skipping
about twice a minute while converging over 15.5 s.

What remains is one manual run by a person on Apple hardware; nothing
deterministic is left to automate. Decided 2026-10-22:

- Run against the public relay on Chrome and Safari, the two rows the
  issue measured, with a real microphone and 40 ms or more of added RTT. Watch
  the stats panel's audio underrun counter and the latency tab's auto readout.
  Re-record the
  `relay-mic` trace in the same run (`just test audio-quality-record`); the
  checked-in one used Chromium's fake capture device and proves nothing here.
- Keep the production refill policy until that run's measurements exist. A
  ring that runs dry re-stalls and refills to the target; change that only if
  the run shows a cause and a justified alternative beats it. The
  deterministic replay ceilings match the recorded durations. Tighten browser
  rows only from nightly runner measurements, re-measured as
  `test/audio-quality/budgets.json`'s note describes.
- Measure the publisher's audio encoder input-to-output lag in the same run
  using the reporter's instrumented harness; #3518 fixed the known cause, so
  the 7.35 s lag and the 88 to 275 ms/s drift the issue reported stand
  unconfirmed. If drift survives, the suspects are `writeFrame` opening a
  group per audio frame under WebTransport stream credit and the main-thread
  task queue delivering encoder output. Audio now defaults `groupDuration` to
  20 ms. A/B that default against a group of 100 ms or so in the same run.

Decided 2026-10-15: keep this proof in m1 with the existing playback and
A/V clock work. The estimator defaults on; deferring watch-worker does not
defer validation of the current player.

## Related

- [A/V clock](/quest/m1/av-clock.md) - reshapes `SyncInput` around the per-track target this line produces
- [Capped budgets](/quest/m1/audio-jitter-target/capped-budgets.md) - confirms the build-flush fix on the nightlies, no gate on this proof
