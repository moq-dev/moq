# [M] A viewer's share of playback latency is reportable, not just measurable in a test

## Goal

A running viewer reports where its share of the audio delay went (the jitter
buffer and the output device) through its `.echo` feedback snapshot, in both
languages. The numbers a user reads when debugging their own latency are the
numbers the harness grades, because they come from the same place. Decode and
render stay internal; publisher and network stages, and an end-to-end total,
wait for a clock that spans the whole path, and none exists today.

## Plan

The audio quality harness (`test/audio-quality/`) landed on ad-hoc debug
probes, which was the right trade to get it running. This quest promotes them.

- Decided 2026-10-08: scope to the viewer's stages. No clock spans
  publisher to viewer, so the harness reports end-to-end as null.
- Decided 2026-10-30: wait for both feedback reporters (see Required), then
  fill the fields from each reporter's tally rather than adding a second
  per-player readout the reporters would have to fold in later.
- Decided 2026-10-30: replace the producer-less scalar `latency` on
  `hang::echo::Rendition` (`rs/hang/src/echo.rs`), `Echo.RenditionSchema`
  (`js/hang/src/echo.ts`), and `ViewerRendition` in
  `drafts/draft-lcurley-moq-hang.md` with an object of stages:
  `latency: { jitterBuffer?, decode?, render?, device? }`, each integer
  milliseconds and absent when unmeasured. The schema is unreleased, so this
  breaks no one.
- Decided 2026-10-30: export only `jitterBuffer` and `device`. The decoder
  runs inside the jitter-buffer span, so `decode` is zero while the ring holds
  anything, and `render` is a fixed ring-to-graph handoff (one 128-frame
  quantum in the browser); neither is something a viewer can act on. They stay
  unmeasured in the harness schema.
- Decided 2026-10-30: leave the publisher snapshot (`rs/hang/src/stats.rs`)
  alone. Every viewer stage belongs in feedback, and publisher stages wait for
  a spanning clock.
- Both languages, matching names, per the repo's cross-language rule.
- Switch the harness over, deleting the probes it replaces. Today
  `test/audio-quality/clients/js/src/probe.ts` reads `sync.out.delay` and
  `AudioContext.outputLatency + baseLatency`, and natively `moq play --metrics`
  writes the presentation target. A ledger with no consumer is how this drifts
  from reality.
- Keep the harness's terms: one duration unit, every timestamp on a named
  clock, and stages as exclusive spans that cannot both claim the same
  milliseconds. Inherit those from the schema rather than restating them,
  so the sum-to-end-to-end identity (with its named `unaccounted`
  remainder) can be added once a spanning clock exists.

## Required

- [Browser reporters](/quest/m2/stats/js.md) - `<moq-watch>` publishes the feedback snapshot these stages join
- [Rust reporters](/quest/m2/stats/rust.md) - `moq play --echo` publishes the feedback snapshot these stages join

## Related

- [QoS](/quest/m2/qos/README.md) - relay-side health, the same idea from the other end
- [Media stats](/quest/m2/stats/README.md) - the hang stats and `.echo` snapshots these stages join
