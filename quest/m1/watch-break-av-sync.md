# [S] A player stays in sync across a demand break

## Goal

After a demand break (the viewer leaves and returns),
`@moq/watch` never plays pre-break audio out of sync with pre-break video: it
either plays them together or jumps straight to live.

## Plan

Found in #5173 (2026-10-10): in one reattach sample of `just test media`,
after the break the player played the old pre-break audio about 1.6 s apart
from the old video before jumping to live. Decided 2026-10-10: plan it as a
player bug.

Reproduce in the media fixture's reattach case, measuring the audio-video
offset of the first frames presented after the break. Find whether audio and
video take different groups from the relay after the break (the relay hands
the reachable pre-gap GOP by design, per #4914 and #5158) or the player's
sync re-anchors one track and not the other. Fix it at that cause, and assert
the offset in the media check.

Public API: `Sync.wait` gains an optional `AbortSignal`. Wire: none.

### Investigation

On Linux Chromium the unchanged full media fixture passed its positive lifecycle
run and all negative controls. The old reattach case measured sync only after
reaching live, leaving the reported pre-live failure outside its checked window.
The draft moves the resume sync check into that catch-up window. It excludes the
held silent preview and requires each advancing audible pair to stay within the
existing one-tone-step tolerance. The negative unit fixture carries the reported
1.6-second stale-audio offset.

A passing run does not establish the root cause or fix the player. Retain this
quest until the new early assertion captures a failure with container group and
shared-reference evidence.

With the early assertion enabled, the positive lifecycle including reattach
passed (30.3 fps, median skew 0 ms, p95 200 ms). The full run then stopped in
the offset negative control on its unrelated audio-tone completeness floor
(68/81 samples, 84% versus 90%). Preserve that validation limit.

Astra iteration reproduced a related shared-clock defect with mocked time: when
an audio reset cleared the reference, `Sync.wait` returned normally and the video
decoder presented its parked frame. Decided 2026-10-11 (no public API change):
`Sync.wait` stays `Promise<void>` and holds a parked wait across a reset until
the next frame re-anchors the clock. Video drops a frame whose wait spanned a
reset, keyed on an `@internal` reset epoch (stripped from the published types),
which also covers a reset and same-value re-anchor in one turn. `Sync.wait`
gained an optional `AbortSignal` (additive), which the decoder passes so a
closed track releases its parked waits. Focused
regressions fail before and pass after the fix.

The media harness now keeps early-resume grading active until audio is running
and unstalled. A moving silent preview no longer ends that grading. Existing
skew and timing bounds remain unchanged.

This does not yet establish the cause of the original 1.6-second reattach sample.
Keep the quest until real-media evidence connects that failure to the fixed path
or independently demonstrates that it is resolved. Frames already waiting for
presentation are covered; queued WebCodecs outputs need separate evidence.