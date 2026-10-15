# [S] js -> go interop never reads a rewound frame

## Goal

The `js -> go` interop cell never fails with moq-mux's "frame timestamp ... is
below the previous group's start", fixed at whichever side produces it. If the
browser publisher emitted a group that starts before its predecessor, that is
a publisher bug: rewinds are unsupported, so the publisher must never write
one. If the frames were in order and the Go subscriber's consumer saw them in
the wrong order, that is a consumer bug.

## Plan

Seen once, in a local `just test interop --all` on #5546 (2026-10-15): one
`js -> go` cell failed with that error and the rerun passed every cell. Not
found in CI: the 13 failed Interop runs on 2026-10-15 and the nightlies of
2026-10-13 to 2026-10-15 do not contain it. The message comes from moq-mux's
container consumer (`rs/moq-mux/src/container/mod.rs`), which the Go client
reads through moq-ffi. The JS publisher is a real browser encoder
(`test/interop/clients/js`), and js/hang's producers already refuse a frame
below the previous group's start when it is written. So a rewind reaching the
consumer suggests either a JS write path that skips that check, or groups
reordered or skipped on the subscribe side.

Reproduce first: loop the `js -> go` cell under CPU load, and keep the
timestamps and group sequences both sides saw. Fix the cause, never by
relaxing the consumer's check or retrying the cell. Land a regression at the
layer that was wrong.

Public API: none expected. Wire: none.

## Related

- [Refuse rewinds](/quest/m1/refuse-rewinds.md) - makes every hop refuse a rewound group, which would move this failure to the publisher
- [Resume reorder](/quest/m1/resume-reorder.md) - a late group header on the subscribe side, one candidate cause
