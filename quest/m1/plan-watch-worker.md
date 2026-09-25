# [L] Plan: where the watch worker lives and who spawns it

## Goal

A measured decision on how `@moq/watch` moves its heavy lifting off the main
thread, recorded by rewriting [Watch worker](/quest/m1/watch-worker.md). The
jank harness and the N-player sweep that decided it land and run nightly; the
prototypes themselves are discarded.

The outcome being chased is jank immunity: a long main-thread task (app JS,
layout, a React commit) never stalls or drops video and never underruns audio.

Boundaries: watch only. Publish and encode stay where they are, but the chosen
shape must let a later publish handle share the worker's connection, so room
keeps one session for publishing and watching.

## Plan

Prototype both models on the same pipeline and measure them against each other:

- **Invisible worker, backend handles.** One lazily created worker per page
  owns every connection (a worker-side pool, like today's per-page pool), the
  subscription, container parsing, both decoders, `Sync`, and an
  `OffscreenCanvas` per player. Main-thread objects are thin handles whose
  signals only cross the boundary on demand: nothing is posted until a main
  subscriber or effect reads a signal, and posting stops when the last one
  leaves. This is the preferred ergonomics, since most applications will never
  spawn a worker themselves.
- **Application-spawned workers.** The composable classes (`Video.Decoder`,
  `Renderer`, `Sync`, `Audio.Decoder`) become DOM-free and run in any worker
  the application creates; `Player` is one such composition with a bundled
  worker. Rendering, decoding and pacing are then shared code, and only the
  spawning differs.

Settled constraints either way:

- `AudioContext` and the render worklet stay on main. The worklet's ring
  endpoint (the `SharedArrayBuffer` ring, or a `MessagePort` straight to the
  worklet on the postMessage path) is handed to the worker so decoded PCM
  never transits main, and the playhead reaches `Sync` the same way.
- `Sync` lives in the worker. Main-thread readers (captions, UI) observe it
  through the bridge.
- `IntersectionObserver`, `visibilitychange`, and the element's size stay on
  main and reach the worker as inputs. A canvas can be transferred only once,
  so a replaced canvas element is a new transfer.
- `renderer.out.frame` goes away. Its one reader, `Player`'s paused-poster
  gate (keep video enabled until a frame is painted, then stop), moves with
  the renderer or reads a painted flag across the bridge. Timestamps and stats
  cross the bridge like any other signal.
- The cross-thread signal bridge is a generic addition to `@moq/signals`, not
  private to watch. Prove its shape here.

The harness drives a real browser, injects main-thread busy loops of varying
length, and counts stalled or dropped video frames and audio underruns. The
sweep runs N players over one relay and records CPU, memory, and delivery
against N, per the fan-out rule. Follow the artifact conventions of
[Browser benchmarks](/quest/m1/browser-benchmarks.md) and reuse the existing
browser harness pieces instead of starting another.

The rewritten implementation quest names the chosen model, the public handle
shape of `Player` and `<moq-watch>`, what happens to the composable classes,
the worker bundling (the publish capture worker's `?worker&inline` is the
precedent; note the CSP `worker-src blob:` consequence), and the follow-up
quest for moving publish onto the same worker if the handle model wins.

## Related

- [Watch worker](/quest/m1/watch-worker.md) - the implementation this rewrites
- [Browser benchmarks](/quest/m1/browser-benchmarks.md) - artifact conventions; may absorb the harness later
- [A/V clock](/quest/m0/plan-av-clock.md) - the `Sync` shape the implementation moves; the prototype can pace against today's
