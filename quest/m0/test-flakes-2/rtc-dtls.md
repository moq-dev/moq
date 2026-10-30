# [S] moq-rtc's session-end test survives load

## Goal

`a_session_ending_without_a_delete_ends_its_tracks` in moq-rtc stops failing
with a DTLS handshake timeout when the host is busy, fixed at its cause.

## Plan

Seen during [moq-dev/moq#6987](https://github.com/moq-dev/moq/pull/6987)'s
local `just check`: it failed once under load, then passed alone and on the
full rerun. Find what bounds the handshake by wall clock (a real timer in the
test or in the DTLS path) and drive it on the simulated clock, or measure it
from progress rather than elapsed time.
