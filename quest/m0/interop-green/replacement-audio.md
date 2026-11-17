# [M] Restore the replacement media case on Firefox

## Goal

`just test media --media-cases replacement --media-engines firefox` passes every run, and the
default media run on Firefox runs it again. Remove the quarantine off Chromium in
`test/interop/clients/js/media.ts` and `test/interop/README.md`.

## Plan

Chromium runs the case again by default. Its audio no longer goes silent across the groups a
starved viewer skips: the restarted decoder is re-primed on the frames before each gap, and a gap
lands what the decoder still holds instead of resetting it away.

On Firefox, the fallback (`live incumbent fallback plays`) misses its 1039 ms bound about one run in
five on an unloaded host, as often before those fixes as after: one failing run had neither a
picture nor started audio, another a picture with the audio stalled. Root-cause it on Firefox first,
reading the player's state across the fallback. WebKit was never run; check it once Firefox passes.

Public API: none. Wire: none.
