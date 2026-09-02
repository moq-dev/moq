# [M] moq import ts: a truncated or spliced Opus PES ends the session

## Goal

A damaged, truncated, or spliced Opus PES costs the frames it covers and
nothing else. A PID that never parses as Opus still fails loudly once the
resync budget is spent.

## Plan

`OpusStream::write` in `rs/moq-mux/src/container/ts/import.rs` treats any
malformed control header as fatal: `Opus access unit exceeds PES payload` when
a PES is cut short of the length its header promises, and
`invalid Opus control header sync` when the bytes after a splice are not a
header. Both propagate out of `Import::decode`, which callers treat as
terminal. A PES need not be damaged in transit to reach this: `handle_pes_start`
flushes whatever is pending when the next PES begins, so a wrap, a dropped
packet, or a capture that starts mid-stream hands a short PES to `write` as if
complete. Opus is excluded from `Resync` and from the carry-across-PES class
that the legacy codecs got in #2751 and #2823.

- Route Opus through `Resync`: scan for the control-header sync (`0x7f` then
  `0xe0` under mask), confirm by parsing the header and finding the next
  header at the promised end before publishing, and keep the 64 KiB budget so a
  PMT that mislabels a PID as Opus does not become a silent dead track.
- Include Opus in the carry class, so a control header split across a PES
  boundary is reassembled and a truncated tail is carried rather than errored.
- Tests: loop `test_data/opus.ts` at every packet boundary (the reproduction in
  the issue) and import each without error; a PID of non-Opus bytes labelled
  Opus exhausts the budget and fails.

## Closes

- [#2849](https://github.com/moq-dev/moq/issues/2849) - close this issue when the quest finishes
