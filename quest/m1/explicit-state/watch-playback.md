# [M] Watch playback phases

## Goal

`<moq-watch>` counts audio and video stalls, stalled time, and the echo
feedback that grades a viewer as stalled only for a rendition that was
playing and then ran dry. None of these count:

- a retired, failed, or removed audio rendition;
- a switch to `delay="instant"`;
- a video poster waiting for the clock;
- a disabled decoder.

Each decoder's playback and rendition-switching phase is one value.

## Plan

The shared decisions are in the [epic](/quest/m1/explicit-state/README.md).

Bugs to reproduce and fix:

- **Audio** (`js/watch/src/audio/decoder.ts`):
  - `#retire()` clears the consumer but keeps `#playing` (the rendition key)
    and `#played`.
  - `#runStalls` counts whenever the ring reports stalled and
    `#played && #playing`. So switching to instant mode, a decode failure, or
    the rendition leaving the catalog each count a stall that keeps growing
    for as long as the state lasts.
  - `#out.timestamp` and `#out.stalled` also outlive the ring that fed them.
  - Recommended: `{ kind: "stopped" } | { kind: "running", key, consumer } |
    { kind: "failed", failure }`, counting only in `running`.
- **Video** (`js/watch/src/video/decoder.ts`):
  - The min-delay `DecoderTrack` posts a poster frame before the clock
    reaches it, and `#runActive` sets `#played` for any frame, poster
    included.
  - With a delay over the 500 ms buffering timer, every cold start or resume
    counts as a stall, which contradicts the comment that the wait for the
    first picture is no stall.
  - `#runBuffering` also returns early when the decoder is disabled and
    leaves `stalled` set, so the buffering spinner can stay on with
    `visible="never"`.
  - Recommended: separate poster and playing phases, counting only the move
    from playing to stalled.

Also sweep the video decoder's rendition switching (`#active`, `#pending`, a
closure-local `pending`, `#selected`, `#failed`), which keeps two copies of
"pending".

Check whether turning on `lookahead` mid-stream re-enters cold startup
anchored at the stream's first picture (`js/watch/src/sync.ts`, `started` is
only kept in buffered mode). If it does, fix it with the sync clock's phase.

[moq-dev/moq#8058](https://github.com/moq-dev/moq/pull/8058) (audio
time-stretch) rewrites much of the audio decoder. This child doesn't wait on
it, so expect a merge conflict.

## Related

- [Zero steady-state audio underruns](/quest/m1/audio-jitter-target/watch.md) - edits the same audio decoder and reads its stall counter
