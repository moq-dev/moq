# [XS] A drill names a shaper that stopped

## Goal

When a relay drill's shaper stops forwarding, the failure says so directly
instead of surfacing later as a status timeout.

## Plan

`Drop for Path` in `rs/moq-relay/tests/drills.rs` prints only the shaper's
counters; only `verify()` reports the shaper's error. Surface that error
when a drill fails. Found while chasing #5796.

Public API: none. Wire: none.
