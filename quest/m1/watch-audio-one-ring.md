# [M] js/watch: audio plays through the postMessage ring only

## Goal

`@moq/watch` has one audio ring: decoded PCM reaches the render worklet over
`port.postMessage`, whether or not the page is cross-origin isolated. The
SharedArrayBuffer ring, its selection, and every test axis that exists only to
exercise it are gone, and the remaining path has unit coverage of its own.

Boundaries: playback only (`js/publish` capture already uses postMessage). No
change to the ring's playout behavior or budgets beyond what removing the
isolated rows implies.

## Plan

Decided 2026-10-22 (maintainer), from 7 nightly runs (Oct 16 to 22, 84 paired
Chromium rows per ring, none void): the isolated ring bought nothing measurable.
Means for isolated vs plain were underrun 19 vs 12 ms/min, discarded 61 vs
38 ms/min, silence share 0.004 vs 0.004, target p50 201 vs 197 ms and p95 221
vs 219 ms; per-row medians matched on nearly every profile. The looser plain
ceilings in `test/audio-quality/budgets.json` are ceilings, not measured gaps.
Production embeds are rarely isolated (moq.dev serves the postMessage path,
and nothing we ship sets COOP/COEP), so the SharedArrayBuffer ring was a second
path to test for a minority of pages. Promoting isolation was rejected: even
Chrome's one-header `Document-Isolation-Policy` must be set by every
integrator on their own HTML, and nothing here showed a gain worth asking for.
CI injects no main-thread jank, but decoding and ring writes run on main in
both paths, so a long task stalls them alike.

[moq-dev/moq#6425](https://github.com/moq-dev/moq/pull/6425) merged before
the pre-removal comparison. The nightlies compare listener quality only.
The required one-off headless Chromium CPU/allocation A/B at 1, 4 and 16
players is recorded with source snapshots and raw windows in
[the measurement report](/test/audio-quality/ring-ab.md). All 54 corrected
windows completed with advancing playheads and no underruns. The initial
relative-timer fixture underfed the device and its measurements were withdrawn;
the corrected fixture paces from the AudioContext clock.

The removal remains unfinished. At 16 players, the paired main-thread CPU
increase for postMessage was a median 9.638 ms/s (0.964 percentage points of
one CPU core), versus an A/A median 0.684 ms/s; median sampled JS allocation
was 687.705 versus 544.690 KiB/s. These isolate ring overhead, not full player
cost: decoding, network, Decoder/Sync effects, separate player AudioContexts,
worklet-thread costs and native backing-store/serialization allocations are
outside the comparison. Both rings remain selected as before.

“Materially more” has no settled numeric definition. Before deleting, the
maintainer must judge this absolute cost against the listener-quality evidence
and maintenance simplification: accept the increase and continue the removal,
or retain both paths and revise when to remove the shared one. Do not treat any
measurable difference as an automatic failed goal, or infer a threshold. Keep
this tested checkpoint draft until that tradeoff is settled. The surviving
path still gets the [watch worker plan](/quest/m2/plan-watch-worker.md)'s
N-player sweep; this one-off comparison does not replace it.

Remaining removal after that decision:

- Delete `js/watch/src/audio/shared-ring-buffer.ts` and its test,
  `SharedAudioBuffer`, `supportsSharedArrayBuffer`, the `createAudioBuffer`
  selection (construct the postMessage buffer directly), the `init-shared`
  message, and the worklet's shared branch. Fold what was post-only in
  `render.ts` and `render-worklet.ts` into the single protocol.
- `replay.ts` drops `Options.ring`; `playout.test.ts`, `replay.test.ts`, and
  `buffer.test.ts` drop their ring parametrization.
- Keep and adapt the postMessage coverage in `buffer.test.ts` and
  `render-worklet.test.ts`: transfer ownership and control ordering, cancellation
  on close, backpressure/reset epochs, truncation, and latency resizing. These
  exercise the real buffer and registered processor independently of removal.
- Delete `js/common/vite-plugin-isolate.ts` and its use in `demo/web` and
  `demo/boy`.
- Drop the audio-quality `ring` axis entirely, not only its `isolated` value:
  `pulse` already implies the native runtime, so rows are keyed by runtime,
  codec, rate, and profile. Remove `--rings`, the `/isolated/` and `/plain/`
  serving, the `ring` void, the schema field, the `ring` key in
  `budgets.json` (keep the plain ceilings as the browser rows'), and update
  `.github/workflows/nightly.yml`, `audio-quality.yml`, `run.sh`, and the
  README. `record.ts` keeps COOP/COEP: it isolates for 5 us
  `performance.now()` precision, not to pick a ring.
- Rewrite the quests that describe both rings to the one ring:
  [A/V clock](/quest/m1/av-clock.md),
  [jitter-target watch](/quest/m1/audio-jitter-target/watch.md),
  [capped budgets](/quest/m1/audio-jitter-target/capped-budgets.md),
  [time stretch](/quest/m2/watch-audio-time-stretch.md), and
  [plan watch worker](/quest/m2/plan-watch-worker.md) (its non-isolated
  `MessageChannel` design becomes the only one).

Public API: none; the ring classes are not exported from `@moq/watch`. The
worklet protocol is internal. Wire: none.
