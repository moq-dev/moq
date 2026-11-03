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
fixtures retain their original arrivals with exact 1024-sample durations.
`relay-mic` was re-recorded on 2026-11-03 from a real microphone (Chrome 156 on
a MacBook Air, built-in microphone, `--mic-broadcast`): zero underruns, skips,
or stalls, an 80 ms p95 target, and 3.5 s convergence, over a 2 ms recorder
RTT. Its replay ceilings were re-measured to match. The AAC recordings still
refill: worst stalled share is 1.7% (`relay-bbb`), with `local-aac` skipping
about twice a minute while converging over 15.5 s.

The nightly browser rows are not clean in steady state either. On the four
whose artifacts were still kept on 2026-11-02 (2026-10-30 to 11-02), AAC
`wide` (40 ms one way, 40 ms sigma) ran its ring dry one to four times a minute
after converging at a 323 ms target, most gaps 100 to 340 ms. Opus `wide`
did so on two nights and Opus `mild` on one.

### The 2026-11-03 run

Run by an agent on the maintainer's MacBook Air (macOS 26.6.2) at `84a0c1ede6`
plus this branch, against `cdn.moq.dev/anon`, with the built-in microphone
published by `<moq-publish>` in Chrome 156 and played by the harness page
(`index.html`) in a separate Chrome 156 profile and in Safari 26.6.2. The
added RTT was a userspace UDP relay on the laptop holding each packet 20 ms
each way, reached through Chrome's `--host-resolver-rules`, so TLS still
validated against the real relay. Safari cannot be redirected that way and
negotiated WebSocket, so its rows ran on the direct path; the laptop has no
Network Link Conditioner and no passwordless sudo. "Underruns" are the stats
panel's counter, which counts each re-stall and each short quantum, after the
first 30 s; "auto delay" is the latency tab's readout over the same window.

| Row | Transport | RTT min | Underruns after 30 s | Skip-aheads after 30 s | Auto delay p50 / max |
| --- | --- | --- | --- | --- | --- |
| Chrome, direct, 20 ms groups | WebTransport | 5 ms | 9 in 88 s (6.1/min) | 0 | 40 / 60 ms |
| Chrome, +40 ms, 20 ms groups | WebTransport | 47 ms | 26 in 149 s (10.5/min) | 1 (43 ms) | 100 / 120 ms |
| Chrome, +40 ms, 100 ms groups | WebTransport | 47 ms | 9 in 149 s (3.6/min) | 0 | 100 / 120 ms |
| Safari, direct, 20 ms groups | WebSocket | 5 ms | 5 in 118 s (2.5/min) | 0 | 120 / 140 ms |
| Safari, direct, 100 ms groups | WebSocket | 5 ms | 12 in 119 s (6.1/min) | 0 | 120 / 140 ms |

The Goal is not met: every row underruns in steady state, though skip-aheads
are nearly gone. The cause is the estimator's quantile, not the browser.
Converted to traces by the README's recipe and replayed, each row's
arrivals reproduce its live re-stalls and short quanta almost exactly
(Chrome +40 ms: 22 and 11 live, 22 and 11 replayed). In the replay the ring sits
at the target less one frame, and each run-dry is one frame arriving later
than that cushion: the target is the 95th percentile of each 500 ms
interval's worst delay, so about 5% of intervals, some six a minute, hold
such a frame. With no concealment or time stretch each one is an underrun.
Raising `QUANTILE` in a replay sweep of the five rows cuts the rate but not
to zero: 0.99 gives 1.0 to 3.5 a minute at a 60 to 200 ms p50 target, 0.995
gives 0 to 3.0 at the same targets, against 5.6 to 13.2 at 40 to 120 ms
today. The run found no defect in the player to fix here.

Publisher, measured with `AudioEncoder` wrapped in the publishing page:
input-to-output lag on live input is p50 0.9 ms and p99 1.1 ms, with drift
under 0.04 ms/s at both group sizes, across five demand sessions of 2.5 to 3
minutes. The 7.35 s lag and 88 to 275 ms/s drift of
[moq-dev/moq#3477](https://github.com/moq-dev/moq/issues/3477) did not recur.
The only large lags, 260 to 360 ms, are each session's first frames: with no
subscriber the publisher retains up to 30 s of captured audio
(`js/publish/src/audio/backlog.ts`) and encodes it as history when demand
arrives, which the watcher discards. The 100 ms groups halved Chrome's rate
at +40 ms but more than doubled Safari's on the direct path, so the run gives
no reason to change the 20 ms default.

Decided 2026-10-15: keep this proof in m1 with the existing playback and
A/V clock work. The estimator defaults on; deferring watch-worker does not
defer validation of the current player.

## Waiting

- Maintainer: decide the Goal, since zero underruns after convergence is not
  reachable at `QUANTILE = 0.95` without concealment. Options: (a) restate the
  Goal as the rate the quantile implies (about six re-stalls a minute at most,
  no steady-state skip-aheads), and leave zero to [Time
  stretch](/quest/m2/watch-audio-time-stretch.md); (b) raise `QUANTILE` in
  `doc/concept/audio-jitter.md`, both implementations, and the corpus, for
  2 to 4 times fewer underruns at 20 to 80 ms more latency, still not zero;
  (c) pull time stretch into m1 as this quest's fix. Recommended: (a), then
  delete this quest once Safari is measured with added RTT.
- Maintainer: Safari with 40 ms or more of added RTT. On the MacBook Air,
  install Network Link Conditioner (Additional Tools for Xcode) or allow
  `sudo dnctl`/`pfctl`, then play `cdn.moq.dev/anon` in Safari with the stats
  panel open for two minutes and record its audio Underruns after 30 s and
  the latency tab's auto delay.

## Related

- [A/V clock](/quest/m1/av-clock.md) - reshapes `SyncInput` around the per-track target this line produces
- [Capped budgets](/quest/m1/audio-jitter-target/capped-budgets.md) - confirms the build-flush fix on the nightlies, no gate on this proof
