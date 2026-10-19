# [S] TS validation gives the same verdict for the same input

## Goal

The TS export picks the same start and anchor for the same input, and the
harness's probes read the whole capture, so a TS validation verdict depends
only on what was sent.

## Plan

Two sources of run-to-run variance, found by #5810 and #5854:

- `Export::fill` walks its tracks in `HashMap` order, and a track's fill
  stops once the jitter buffer is full. Jitter acquisition and the video
  start can complete partway through a pass, before other tracks' available
  frames are read, so the start keyframe varies (0 ms vs 600 ms seen). The
  export should see every track's available frames before it picks the
  start; a deterministic iteration order alone only hides the dependency.
- `test/ts/decode.py` runs `ffprobe` with default probing (5 MB). #5854 fixed
  `holes.py`, where the first MP2 PES 5.9 MB in made ffprobe guess MP3 and a
  repeated timestamp read as a hole; `decode.py` has the same exposure.

Decided 2026-10-17 (maintainer): one quest for the grader, with the CI path
filter as a separate m0 quest. The harness
never gates on a random-loss percentage (see
the root `AGENTS.md`).

Public API: none. Wire: none.
