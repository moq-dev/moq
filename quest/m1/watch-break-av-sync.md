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

Public API: none expected. Wire: none.
