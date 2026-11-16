# [M] Video warmup returns to the catalog and the players

## Goal

A video rendition can declare `warmup` again, as a feature of the catalog
version after moq-hang-04, and every player plays such a rendition through
one decoder: `@moq/watch`, `moq_video::decode`, `moq play`, moq-gst, and the
bindings join one group earlier and hide the warmup after each join or
skip. The entry carries the `version` that makes a 0.22 player skip it
instead of mis-decoding it.

## Plan

Decided 2026-11-16 (maintainer, /quest-plan of the pre-cut audit): video
`warmup` left hang-04 and the players' path was deleted before the cut
([moq-dev/moq#8857](https://github.com/moq-dev/moq/pull/8857) had landed
that path on 2026-11-15), because no producer wrote the field, Firefox and
Safari had never run it, an HEVC refresh rendition fails in Chromium, and a
0.22 player that claimed to read it would mis-decode a later producer's
rendition. It returns here, with the producers this epic adds.

- Restore from history, not from scratch: #8857 is the design (one
  decoder, the container consumer's warmup reach, `Recovery`'s hold, the
  recovery-point SEI Chromium needs), and the PR that removed it before the
  cut is its inverse. Read both, then re-apply by hand against the tree as
  it is.
- The field comes back in the hang draft's next version, with the group
  rule that lets a video group start at a picture from which decoding
  converges. It raises the entry's `version`, not the root's (the
  per-entry rule hang-04 ships, `Entry::version` in `rs/hang` and
  `videoVersion` in `js/hang`), so a player that cannot play it keeps the
  other renditions.
- Audio `warmup` never left; reuse `moq_mux::container::Playout` and the
  JS consumer's `warmup` reach as they are.
- The draft says which engines play it. The Firefox and Safari check in
  [second decoder](/quest/m4/intra-refresh/dual-decoder.md) needs a real
  intra-refresh stream, so it runs after the
  [H.264 import](/quest/m4/intra-refresh/h264-import.md); until it passes,
  the draft's text and `doc/lib/js/watch.md` name Chromium only, and the
  new catalog version is not published.

Tests: the ones #8857 carried, restored with the path: a cold join and a
skip of a warmup rendition each show one clean picture after the warmup,
in the browser decoder and in `moq_video::decode`.

Public API: additive in Rust, JS, and the bindings (the video `warmup`
field and the decoders' handling). Wire: the next hang catalog version.
