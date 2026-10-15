# [S] The WASM IETF read case holds on CI

## Goal

The WASM suite's `ietf: reads a published broadcast` case (`test/wasm/src/main.ts`)
passes on loaded CI runners, fixed at its cause. It still proves that a
browser session over moq-transport reads whole groups, in order, from a
published broadcast.

## Plan

Seen twice on the WASM job, both as `read 0 whole groups in 8 attempts, want 2`:
run 38778431765 on #5577 (2026-10-15) and run 38607947479 on
`quest/m0/ci/js-select` (2026-10-14). The lite and SETUP flavours of the same
case passed in both. In the first log, group 0 arrives, then about 500 ms
pass with nothing, then groups 1 to 5 arrive within 6 ms and 6 to 8 soon
after, and the reader hits its attempt bound having read none whole.
`expectGroups` treats a group the live edge passed as a legal skip (`isOld`,
from the default `Latency::REAL_TIME`), so every one of those groups was
retired before it was read.

Find out why the IETF path stalls and then delivers in a burst while lite
does not, and whether retiring a group that arrived whole within the budget,
only because a newer header landed in the same burst, is correct real-time
behaviour. A product bug is fixed in the product. If the behaviour is right
and the case asks for something real-time delivery does not promise, change
what the case asserts, not its attempt bound or timing. Reproduce under CPU
load before deciding.

Earlier WASM failures of the same case (2026-10-02 to 2026-10-12) failed with
`unsupported`, a different signature; confirm it is gone rather than folding
it in.

Public API: none expected. Wire: none.
