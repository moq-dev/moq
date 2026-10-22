# [M] Parse QMux records without copying each remaining suffix

## Goal

QMux record decoding does work proportional to bytes and frames rather than
repeatedly copying the entire unparsed suffix. Retaining a small frame must
also not unnecessarily pin a large record allocation.

## Plan

P2 performance/resource issue at main 6369eca.
`js/qmux/src/frame.ts::take` clones both the requested payload and remaining
buffer. `decodeRecord` repeats this for each frame, even empty STREAM frames.
A legal 16,380-byte record containing 5,460 empty STREAM frames caused
44,709,210 copied bytes before dispatch, measured by instrumenting
`Uint8Array.slice` around the real decoder. Smaller records of 1,023 and
4,095 bytes copied 173,910 and 2,792,790 bytes. The largest took about 10.9ms
on the audit host; deterministic copied-byte growth is the evidence, not
that machine's timing. Empty frames on an existing stream need no data credit.

The in-tree implementation is new since release. Release used the external
`@moq/qmux` package, whose old source was not compared, so this is not a
demonstrated main-only regression.

Use a cursor/view for parsing and make payload ownership explicit at the
retention boundary. Check every frame variant using the shared helper.
Benchmark record size and frame count independently, including empty/control
frames, truncated lengths, and retained tiny payloads. The current consumers
are QMux WebSocket fallback receive loops. No public API or wire change is
expected.

## Related

- [QMux no-wait opens](/quest/m1/qmux-no-wait.md) - distinct stream-creation credit behavior
