# [S] The T-STD harness covers MB overflow and an AAC broadcast reference

## Goal

`just test ts-tstd` has a negative control that overflows the video multiplex
buffer (MB), and a positive reference that carries AAC the way a broadcast
encoder does. Every buffer and codec the export emits then has a control on
each side.

## Plan

Found in #4643. Grading against the declared HRD made MB about 3.6 MB on the
Kyrion capture, more than its 4 s holds, so no control reaches MB any more.
The only positive reference carries MPEG audio, but the export emits AAC.

- MB: a longer clip with no declared HRD (level defaults), restamped to a
  burst, which overflows MB and passes as captured.
- AAC: a shareable broadcast-style reference whose AAC passes B.
  - FFmpeg's default 0.7 s audio lead overflows the 3,584 B AAC buffer, so try
    its mux delay options first.
  - If no generated clip passes, ask t0ms whether a short AAC capture can be
    shared.
- Each control fails or passes for the stated reason, and `interop.yml`
  runs them.

## Required

- [T-STD TS export](/quest/m1/tstd/README.md) - `just test ts-tstd` and `tstd-controls.py` land with it (#4645)
