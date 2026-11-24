# [M] Room participants survive your own connection blip

## Goal

When the local user's relay connection drops and comes back, the remote
participants already on screen stay in place and keep their players, and are
removed only once their broadcasts are withdrawn, not when the user's own
connection blips.

## Plan

Found 2026-11-10 while charting the room flow ([moq-dev/moq#8447](https://github.com/moq-dev/moq/pull/8447)),
not yet encoded as a rule or reproduced by a test: every remote participant
drops off the screen on the local connection's offline event and comes back
as a new player on reconnect, which re-lays out the room and restarts every
video.

Reproduce it first as a test in `js/room`. Recommended fix: keep remotes
through an "offline" announce event and drop them only on "absent" (the
broadcast withdrawn), so a reconnect resumes the same player. Add a rule to
`js/room/verify/Room.tla` that a remote participant whose broadcast is still
live stays on screen across the user's reconnect.

Related: [a room keeps a participant while one of its aliases is
announced](/quest/m3/room-alias-withdraw.md) changes the same attach and
detach code; whichever lands second adapts.

Public API: none. Wire: none.
