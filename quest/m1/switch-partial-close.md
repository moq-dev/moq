# [M] A replaced copy's short group continues from the replacement

## Goal

A group that a replaced copy closes short (FIN without GROUP_END) continues
from the replacement at the next frame, so the reader gets every frame the
group holds, as if read from one route. The group ends early only when no
route can supply more. Rust and `@moq/net` match.

## Plan

Found by the 2026-11-20 audit of the subscription rules before the lite-07
cut, and reproduced in JS. In the resume test "a stalled group continues from
the replacement at the next frame", have the old copy close its one-frame
group with `finish()` before `swap()`, under a five-minute budget. The other
copy holds three matching frames, yet the reader ends the group after frame 0.

- Rust: a bare FIN finishes the received group (`recv::Group::end(None)`,
  `rs/moq-net/src/recv.rs`), and `poll_resumed` (`model/group.rs`) treats a
  finished read as needing no route ("Read to its end").
- JS: `#readFrame` (`js/net/src/group.ts`) returns the end: "The copy finished
  the group: a replacement asked for meanwhile is moot".
- Only when the reader parks on the next frame before the FIN lands does
  `Recover::wants` move it to the replacement. Which happens depends on
  arrival order.

Recommended: a read that reaches a closed group's end on a copy the front has
replaced, or while another route may hold more, recovers the next frame from
the replacement as a stall does. It ends the group only once a copy holds it
whole, or the replacement also has nothing past that frame. A group read to a
GROUP_END is whole and ends as today.

Add the reproduction in both languages, and the case to the resume models
(`/verify-resume`), whose oracles should require every frame some route held.

Public API: none expected. Wire: none.
