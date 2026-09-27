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

## Related

- [Moxygen compatibility](/quest/m1/moxygen/README.md) - subgroups stay out of scope; only the blast radius is in
