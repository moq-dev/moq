# [S] The ingest fuzzer's stream-size bound holds

## Goal

`Fuzz (ingest)` passes again: the crash input it found on 2026-11-19 either
exposes a bug in lite ingest that gets fixed, or a bound in the harness that
was wrong and gets corrected, and the input joins the corpus as a regression.

## Plan

Found by the 2026-11-19 triage of run [44562804083](https://github.com/moq-dev/moq/actions/runs/44562804083):

```
0 frames of 0 bytes account for at most 27 of a 101-byte stream
```

The assertion is in `rs/moq-net/src/fuzz/ingest.rs:193`; the input is the
job's `crash-278598a8...` artifact. Decide first whether ingest consumed
bytes it should have rejected or the harness undercounts what a stream may
spend on non-frame data (GROUP headers, GROUP_END, padding).

Public API: none. Wire: none expected.
