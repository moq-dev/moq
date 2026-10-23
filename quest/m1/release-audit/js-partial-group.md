# [S] JS refuses an unrequested partial lite group

## Goal

A JS lite subscriber refuses a group stream whose `frameStart` is above 0 as a
protocol violation, closing the session, instead of exposing its first frame
as frame 0.

## Plan

P2, inherited from release (lite-06 added the field; JS parses and ignores
it). The 2026-10-22 subscription audit injected a lite-07 group declaring
`frameStart: 5` at main 1f5caa7026; its first frame read back as frame 0. The
group receiver in `js/net/src/lite/subscriber.ts` builds every group from
frame 0 and checks the offset only for compressed groups. A partial group then
looks complete, corrupting the frame positions resumption and deduplication
rely on.

Decided 2026-10-22: refuse rather than model the offset. A JS subscriber never
asks for a mid-group start, so a nonzero `frameStart` is a peer bug. Raise
`ProtocolViolation`, folding the compressed-group check into the same rule.

The violation must close the session, as `ProtocolViolation` promises, so a
hostile peer cannot repeat it on every stream (found in review of the planning
PR). Today `runGroup` catches every error, closes the group, resets the
stream, and returns, so nothing reaches the dispatch in `lite/connection.ts`.
The existing check for a group past the track's declared end throws the same
way and is swallowed too; fix both, letting the violation escape `runGroup`
while ordinary group errors stay stream-scoped. Test that each closes the
session.

Public API: none. Wire: none.
