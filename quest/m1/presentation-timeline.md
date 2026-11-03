# [L] moq-mux presents a broadcast on the publisher's timestamps

## Goal

moq-mux exposes the playout timing that `moq play` uses today, so any
consumer plays a broadcast's audio and video on the publisher's timestamps
with one delay for the whole broadcast. `moq play` and
[moq-dev/studio's broadcast sources](https://github.com/moq-dev/studio/blob/main/quest/m0/broadcast-sync.md)
both use it, so lip sync holds through jitter and stalls in both rather than
in `moq play` alone.

Non-goal: decoding or output devices. The timeline schedules frames;
callers keep their decoders, windows, and speakers.

## Plan

Decided 2026-11-02 (maintainer, from moq-dev/studio's broadcast-sync): lift
the timing out of `rs/moq-cli/src/play/timeline.rs` into moq-mux, which
already owns the publishing side's broadcast clock (`clock.rs`) and the
`Pacer` the timeline folds every frame into (`pace.rs`). Lift it rather than
copying it into the studio.

- **One anchor:** while audio plays, only audio moves the anchor, and video
  reads it and holds each frame until its audio-aligned time, as `moq play`
  does now. A broadcast with no audio keeps video's own short delay.
- **Delay:** the `--delay auto` target comes from moq-audio's jitter estimate
  (`rs/moq-audio/src/jitter.rs`); the timeline takes that target as an input
  rather than depending on moq-audio, so it stays decode-free.
- **Shape:** a Producer/Consumer split or returned handles, not callbacks
  (AGENTS.md). The video half reports the frame to show at a given instant,
  so a caller that repeats its last frame rather than stalling (the studio's
  canvas) can.
- **Migration:** `moq play` switches to it with no behavior change, proven by
  its existing timeline tests, which move with the code. The API is new
  public surface, so it lands before the next cut.

## Related

- [Audio jitter target](/quest/m1/audio-jitter-target/README.md) - the delay estimate the timeline takes as its target
- [A/V clock](/quest/m1/av-clock.md) - the browser's equivalent, where the audio playhead drives sync
