# [M] A browser publisher's stall never ratchets its viewers' latency

## Goal

A live camera and microphone publish from `moq.dev/publish` plays at the
viewer's Auto latency target for the life of the broadcast, the way
`bbb.hang` does: the catalog's `jitter` and `delay` settle after startup
instead of climbing, so a viewer's buffered media stops growing. Reported on
Brave on Fedora 44 (Wayland) and reproduced on macOS: the viewer's total
buffer climbed 8.8 s, 11 s, 12.7 s, then 42 s within minutes of going live.

The fix lands at whatever publisher-local stall feeds the estimate. The
catalog rule that `jitter` and `delay` never fall stays as specified.

## Plan

Mechanism, from reading the code on 2026-10-20 (not yet reproduced):

- `Estimator` (`js/publish/src/jitter.ts`) measures each frame's flush
  lateness (`now - timestamp`) and advertises its lifetime maximum spread as
  `jitter` and its trail behind the broadcast's earliest rendition as
  `delay`. `CatalogProducer` (`js/publish/src/catalog.ts`) refuses a decrease,
  as the hang draft requires. One stall on the publisher is therefore
  advertised for the rest of the broadcast.
- The watcher's Auto target adds both catalog terms with no bound
  (`js/watch/src/video/decoder.ts`, `js/watch/src/audio/decoder.ts`), so the
  playhead holds that much behind the live edge and media piles up ahead of
  it. The reported numbers match exactly: the "+Xs" label is the total buffer
  minus the timeline's 4 s width.
- The file source behind `bbb.hang` stamps both tracks from one clock and
  never stalls, which is why it stays near 0.4 s.

Candidate triggers, to confirm by reproduction before fixing:

1. Video encoder backlog. The encode loop (`js/publish/src/video/encoder.ts`)
   never reads `encodeQueueSize`, so a software H.264/VP8 encoder slower than
   real time queues without bound and lateness rises linearly. This fits the
   growth rate best.
2. Audio and video on different clocks. Audio timestamps count samples on the
   AudioContext render clock (`js/publish/src/audio/capture-worklet.ts`);
   video follows the camera clock re-based once on its first frame
   (`js/publish/src/video/processor.ts`). Rate drift between them becomes
   `delay`.
3. A throttled or backgrounded publisher tab.

Reproduce first: publish from Brave or Chromium on `moq.dev/publish` (or a
local `just dev`), log `encodeQueueSize` and each flush's lateness on the
publisher, and record the catalog's per-rendition `jitter` and `delay` on a
viewer over time. Land a regression test with mocked time that reproduces the
trigger, such as a slow mock encoder.

Settled decisions:

- Fix the stall at its source and keep the never-fall rule. Letting the
  estimates decay was rejected: it changes wire semantics in the hang draft,
  Rust `moq-mux`, `js/publish`, and the player for a problem that starts on
  the publisher. If the trigger turns out to be one the publisher cannot
  prevent (a throttled tab, a GC pause), stop and propose a course change
  rather than patching around it.
- No player-side bound on the advertised terms. The player keeps trusting
  the catalog's `jitter` and `delay`.
- If the trigger is the encoder backlog, a real-time encoder drops input
  frames instead of queueing; a pending keyframe request survives the drop.
- If the fix touches `Estimator` itself, mirror it in Rust's
  `moq_mux::catalog::Estimator` (`rs/moq-mux/src/catalog/estimate.rs`).
- The fixed-latency half of the report is already fixed on `main` ([#4162](https://github.com/moq-dev/moq/pull/4162));
  [Release fixed delay](/quest/m0/release-fixed-delay.md) ports it to
  `release`, which is what `moq.dev` runs.

## Related

- [Release fixed delay](/quest/m0/release-fixed-delay.md) - the deployed player also adds the catalog terms to a fixed latency
- [Subscribe update churn](/quest/m1/subscribe-update-churn.md) - the alternating `subscribe update` flood in the same report
- [A/V clock](/quest/m1/av-clock.md) - the player-side clock; the publisher's two capture clocks are this quest's
