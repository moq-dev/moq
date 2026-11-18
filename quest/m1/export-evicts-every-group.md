# [M] A live export reads each group to its end instead of evicting it

## Goal

A container export following a steady live broadcast reads every group to its
clean end: it logs no `current group evicted` and counts no loss while nothing
is lost. Each group's tail frames reach the export.

## Plan

Seen 2026-11-17 while validating
[moq-dev/moq#9049](https://github.com/moq-dev/moq/pull/9049) on `main`: a
local `moq-relay`, ffmpeg pushing 30 fps H.264 with a 1 s GOP over RTMP into
`moq import rtmp`, and `moq export webrtc --listen` from the relay (both on
moq-lite-06, WebTransport). The export logged `current group evicted; skipping
to next buffered group ... error=Moq(Stream(Old))` from
`rs/moq-mux/src/container/consumer.rs` once per group (33 in 33 s), while
Chrome still decoded about 30 fps. Every evicted group calls `lose()`, so the
export's loss accounting reports a loss per group.

Reproduce first, then find which side ends a finished group as `Old`: the
relay hop, `moq-rtmp`'s group writes, or the consumer reading a group whose
successor started. Recent candidates touching this path:
[#8910](https://github.com/moq-dev/moq/pull/8910) (an ordered track reports
the groups it gives up on) and
[#8961](https://github.com/moq-dev/moq/pull/8961) (a group's end in one word).

## Related

- [Export order](/quest/m3/export-order.md) - the same warning after an outage, where the eviction is real
