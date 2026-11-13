# [S] JS byte holes

## Goal

`@moq/net` speaks the lite wip byte offset on FETCH's frame start and group
headers as Rust does, so a JS reader cut off inside a large frame continues it
from the byte it reached instead of failing.

## Plan

Decided 2026-10-27 (maintainer): split out of the JS unbounded groups
quest. This half mirrors
[Byte holes](/quest/m2/unbounded-groups/byte-holes.md).

Decided 2026-10-31 (maintainer, on
[moq-dev/moq#7411](https://github.com/moq-dev/moq/pull/7411)): chunked frame
storage and reads moved into that PR, which removed the JS frame cap. JS now
has `Group.Producer.createFrame` and `Group.Consumer.nextFrame` with
`readChunk`, stores a large frame as 1 MiB segments that shed under the
32 MiB tail, and streams payloads past one segment on lite and
moq-transport. What is left here is the wire:

- Mirror the byte offset on the same lite wip version as Rust, on FETCH's
  frame start and the group stream header, and refill a hole inside a frame the way
  Rust does.
- A consumer that took a frame to stream and loses its copy partway
  through fails today, since resuming at the next frame would drop the rest
  of that frame (`#torn` in `js/net/src/group.ts`). Continue it from the
  serving route's copy at its byte offset, as Rust's `frame::Consumer`
  `Recover` does once Byte holes lands.
- Test a frame several times the cache window through a Rust relay, read
  as chunks, plus Rust/JS interop of a mid-frame refill. Run
  `just test interop --all`.
- If [JS-only @moq/net from Rust](/quest/m5/rs2ts-js.md) is adopted first,
  this shrinks to exposing its byte-offset recovery.

Decided 2026-11-05 (maintainer, /quest-audit): ✅ the byte offset rides
FETCH's frame start plus the group header, not a SUBSCRIBE range end.
SUBSCRIBE ranges are whole groups since 2026-11-03
(Lite-07 FETCH is single-group only,
[moq-dev/moq#8616](https://github.com/moq-dev/moq/pull/8616) and
[moq-dev/moq#7675](https://github.com/moq-dev/moq/pull/7675)), and flights
ride lite FETCH since [moq-dev/moq#7944](https://github.com/moq-dev/moq/pull/7944).
This waits for that quest.

Public API: none beyond the recovery. Wire: the lite wip byte offset, as
Byte holes defines it.

## Required

- [Byte holes](/quest/m2/unbounded-groups/byte-holes.md) - the Rust contract and the lite wire this mirrors
