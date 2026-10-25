# [S] A reserved group survives a route returning to an earlier copy

## Goal

A resumed reader that peeked a group keeps that reservation when its route
flips A to B and back to A, and its next take returns the reserved group
instead of panicking at `resume.rs`'s `unreachable!("the peek reserved a
group")`.

## Plan

Found by the 2026-10-24 pre-cut audit at 151a529670. Reproduced, new on main
(release's splice design has no peek path).

A reservation lives in two places: `Reader.peeked` (a `Candidate`) and the
copy cursor's `Cursor.peeked`. On a return to copy A, `Reader::sync`
(`rs/moq-net/src/model/resume.rs:958-990`) reuses A's copy, raises its floor,
and calls `set_range` (`model/track.rs:7013`), which clears the cursor's
reservation while `Reader.peeked` survives. If the reserved group was evicted
meanwhile, or sits below the new floor, the take hits `unreachable!` (about
`resume.rs:1250`) or the `assert_eq!` after it. In production the peek path
runs when a route copy is itself a front's logical track (origin-to-origin
chaining), and the flip must land between peek and take; the panic is
contained to the session by b685511410.

The audit's reproduction, a unit test for `resume.rs`'s test module: build a
nested logical track, write group 0 on copy A and drain it, write and finish
group 2, `poll_peek_group` it, abort group 2 with `Error::Evicted`, serve
copy B, serve copy A again, then take with both `poll_next_group` and
`poll_recv_group`. It panics in both orders.

Fix direction: the `Candidate` carries the reserved `group::Consumer` itself,
plus a cursor step that commits the sequence, so a take never re-queries a
cursor that `set_range` or `update` may have reset. That deletes the
`unreachable!`/`assert_eq!` pair. The narrower alternative is a `set_range`
that keeps a reservation still inside its new bounds.

Public API: none. Wire: none.

## Related

- [One route phase](/quest/m1/cut-audit/route-phase.md) - the other small resume fix ahead of the cut
- [Resume as producer replacement](/quest/m2/resume-redesign.md) - the redesign that deletes the two-level reservation
