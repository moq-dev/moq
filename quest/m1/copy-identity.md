# [M] Copies of one group identity must agree beyond their bytes

## Goal

Two copies of the same broadcast+track+group (another route, a FETCH answer, a re-offer, a mid-frame recovery) that disagree on anything a reader sees fail the later copy with a protocol violation, instead of readers of each copy seeing different content under one identity. Today `rs/moq-net/src/model/group/content.rs` `row()`, `write_chunk` and the recovery in `frame.rs` compare only size, `inflated` and overlapping bytes; nothing compares a frame's timestamp, its IETF object fields, or the group's final frame count, so a copy with one more frame can raise `largest` after `finish_largest` declared it settled.

## Plan

Found by the 2026-11-07 invariant audit (model-1). Put the timestamp and a digest of the object fields in the shared row, and the group's fin in `Content`; a copy that disagrees, commits past the fin, or finishes short of it gets `ProtocolViolation`. Tests: two copies with differing timestamps, differing fields, and an extra frame. Check the JS counterpart (`js/net/src/group.ts`) and match it in the same PR.

## Related

- [A reader on a failed copy continues on the held re-offer](/quest/m1/copy-recover.md) - also stitches frames across copies
