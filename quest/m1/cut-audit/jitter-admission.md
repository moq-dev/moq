# [M] The export jitter buffer admits only frames due within its window

## Goal

The TS, MKV, and FLV exports' jitter buffer stays bounded by construction:
a track is read only while its newest queued frame is due within the delay
plus its lead, so a track whose timestamps outrun real time is held back in its
source as backpressure instead of queued without bound beside an honest
track.

## Plan

Found by the 2026-10-24 pre-cut audit at 151a529670. Reproduced, new on main
(`jitter.rs` is not on release).

`full()` (`rs/moq-mux/src/jitter.rs:1058`) reports a track full only once
its front frame is due, so future frames are always read and queued;
`bound()` (`:692`) gates only generation decisions; and the drift check
(`:517`) measures the steadiest track. With a 500 ms delay, audio at 1x and
video at 3x, the buffer grew linearly from 500 frames at 20 s to 3,006 at
120 s with no error. A fast track alone does fail, so one bad track stays
unbounded while an honest one sits beside it. Affects `ts/export.rs:442`,
`mkv/export.rs:485`, and the FLV export.

Make admission the bound: a track is full once its newest queued frame (the
back of its queue, or the frame about to be admitted) is due later than now
plus delay plus its lead. Judging the front frame would not work: in the
scenario above the oldest video frame stays near its deadline while newer
frames pile up behind it, and `Stream::fill` keeps reading while
`jitter.full(name)` is false. No frame-count cap. Alternatively charge
a track whose lead keeps growing as a source fault.

Bound bytes too (decided 2026-10-27, maintainer, from a second audit of
main against release). Timestamp distance does not bound memory: frames
that share one timestamp all fall inside the window, and the container
consumer's max buffer
([moq-dev/moq#7056](https://github.com/moq-dev/moq/pull/7056)) only charges
frames the jitter buffer has not pulled yet. So a track is also full while
its queued bytes exceed a per-track byte budget. Like the time bound, it
holds the track back in its source as backpressure, never refuses a frame.
Test a track that packs many frames into one timestamp beside an honest one.

Then evaluate the larger simplification: about a dozen fixes to the export
clock (f85547ffd0, 5d5974f083, 3fdf46160b, e84ffb3de3, a1465d0d00,
11bfa30461, 3cf978cdbf, 493d8eee99, 7a3890cd30, 3b77d23015, 7c4c48d08b,
41f3087e3c) each added a rule about which frames count. With admission as
the single invariant, check whether acquisition, the leapt/lagging arms,
and some of the nine tuning constants can go. Land what the measurement
supports; record what it does not.

Public API: none. Wire: none.

## Related

- [TS fidelity and recovery](/quest/m1/ts-reliability/README.md) - the import side's state ownership
