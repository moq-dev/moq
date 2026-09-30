# [S] Subgroup refusal stays on the stream

## Goal

A peer that sends a non-zero subgroup on moq-transport loses that one stream,
never the session. Every other track on the session keeps flowing.

## Plan

Against moxygen's `moqtest_server`, a track with two subgroups per group ended
the relay's upstream session, and the server reconnected. Our side refuses the
stream today. Whether the session ends because of how we refuse it (the reset
code, STOP_SENDING, or the alias state it leaves behind) or because the peer
reacts badly to a correct refusal is not known yet. Reproduce it first. If the
peer is at fault, say so on its tracker and keep a regression test for our side.

A test with an IETF peer that sends a subgroup 1 stream next to a healthy track
is the check.

Moved from the moxygen line to m0 in the 2026-09-30 audit: a non-zero
subgroup ending the upstream session is exactly m0's "legal input never fails
a session", and moxygen will send it at Seattle interop on 2026-10-12.

## Related

- [Moxygen compatibility](/quest/m1/moxygen/README.md) - subgroups stay out of scope; only the blast radius is in
