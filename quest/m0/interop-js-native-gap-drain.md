# [XS] Drain queued audio before failing on a gap in js-native interop

## Goal

In `test/interop/clients/js-native/subscribe.ts`, hitting an audio `gap`
flushes already-queued contiguous AAC frames out of the decoder first and fails
only if `decoded < want` after draining, so a cell whose requested audio was
already intact does not flake on a later backlog gap.

## Plan

Found 2026-11-27 by replaying `gpt-6.2-sol` (`medium`) on
[moq-dev/moq#9769](https://github.com/moq-dev/moq/pull/9769), which merged
unreviewed under the `low` tier.

- Caused by: [moq-dev/moq#9769](https://github.com/moq-dev/moq/pull/9769)
  (list under `## Caused by` in the fixing PR).
- At `test/interop/clients/js-native/subscribe.ts:211-215`, when Node or Bun
  reads a buffered backlog, the reader can queue 600 ms of contiguous AAC and
  then hit a later gap before `libav` delivers the decoded output. Throwing
  immediately at `next.kind === "gap"` wins `Promise.race` and closes the
  decoder, failing a cell that already received the requested audio.
- Flush the decoder on `next.kind === "gap"` and throw the gap error only if
  `decoded < want` once the flush settles.

Tests: `just check` and the `js-native` interop audio cell pass.
