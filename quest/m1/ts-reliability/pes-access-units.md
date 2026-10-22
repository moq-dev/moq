# [M] TS video import assembles access units across PES boundaries

## Goal

An H.264 or H.265 PES carrying several access units publishes each as its
own frame at its own presentation time, and an access unit split across PES
packets publishes once. A stream whose later access units' timing cannot be
derived is refused loudly, never published with duplicate timestamps.

## Plan

Today `Import` flushes the codec splitter after every video PES ("Each PES
is one access unit", `rs/moq-mux/src/container/ts/import.rs`), so two access
units in one PES publish two frames at the first one's PTS (reproduced in
the 2026-10-22 audit with the existing fixture).

Decided 2026-10-22 by the maintainer:

- Keep one elementary-stream assembler per PID across PES boundaries; a PES
  PTS stamps the access unit that starts in it.
- A later access unit without a PTS of its own derives it from the POC and
  the SPS's VUI timing when the SPS declares both; otherwise the stream is
  refused. Never repeat the previous timestamp.

Extend the T-STD harness's video profile beyond one access unit per PES.

Tests: the same elementary stream repartitioned into one access unit per
PES, several per PES, and access units split across PES, through import and
export, each publishing identical frames and timestamps; a multi-unit PES
without VUI timing is refused.

Public API: none. Wire: none.
