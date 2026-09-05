# [M] moq-net: resume resolves track info from segment zero, not the newest

## Goal

`resume::Consumer::poll_info` reports the track info of the generation whose
frames a reader will actually receive. A track republished on a live path
serves the successor's `priority`, `maxAge`, and `timescale`, not the
predecessor's.

Boundaries: this is the Rust half of the same defect `js/net` fixed by keying
its `TRACK_INFO` cache on the routing front. The wire format does not change,
so no draft moves.

## Plan

`resume::Consumer::poll_info` (`rs/moq-net/src/model/resume.rs`) resolves info
from **segment zero**, while every data read routes to the **newest** segment.
When a broadcast is replaced on a path, those are different generations, so a
subscriber can be handed the predecessor's metadata alongside the successor's
frames.

`timescale` is the damaging field: the reader converts frame timestamps with
it, so a mismatched generation silently rescales every timestamp rather than
failing. `priority` and `maxAge` are wrong but survivable.

`origin.rs`'s `run_front` already re-resolves the successor's info on
`Step::Splice` and then discards it, which is the natural place for the fix to
hang off.

### How it was found

Isolated while fixing the `js/net` half. Against a real relay, a publisher
wrote 1234 ticks on a successor's MICRO grid and the subscriber received
`1000us`: the JS fix alone was not enough, because the relay's own stale
metadata rescaled the frames. With the Rust side still wrong, the same
mismatch is reachable from any client.

### Not covered by an existing tracker

[#2991](/quest/m1/2991-net-coalesce-dynamic-tracks-and-preserve-sequences-across.md)
is about sequence continuity across replacement, not info resolution. #2610's
epoch remedy was removed from the draft by #3225, so there is no wire-level
generation marker to lean on; the fix is local.

## Related

- [#2991](/quest/m1/2991-net-coalesce-dynamic-tracks-and-preserve-sequences-across.md) - sequence continuity across the same replacement
