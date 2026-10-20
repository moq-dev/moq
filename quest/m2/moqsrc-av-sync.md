# [M] moqsrc keeps audio and video in sync through real sinks

## Goal

A test plays a broadcast through `moqsrc` into clock-synchronized audio and
video sinks and measures the A/V skew of what they render. It fails when the
skew exceeds the bound that `just test media` holds the browser to. It runs
at least nightly in CI.

## Plan

[#5556](https://github.com/moq-dev/moq/pull/5556) fixed `moqsrc` alignment and
tested it at the source pads: buffer timestamps and segment events, not
decoded playback.

- Reuse the browser media QA's tone/frame pattern fixture
  (`test/interop/clients/js/media.ts`, `src/pattern.ts`) rather than
  inventing a new marker scheme.
- Read the markers back after decode on a synced pipeline. Today the interop
  gst cell runs `moqsrc ! filesink` with no sync.
- Cover a cold start and a mid-stream join.
- Check that the test fails with #5556 reverted.

Public API: none. Wire: none.

## Related

- [moqsrc reconnect](/doc/bin/gstreamer.md#moqsrc) - redials and resumes on the same pads, a natural second scenario for this check
- [A/V clock](/quest/m1/av-clock.md) - the browser side of A/V sync
