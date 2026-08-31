# [L] Epoch

## Goal

Implement the announcement `Epoch` that moq-lite-06 specifies, so two publishers
colliding on one path resolve instead of both serving.

## Plan

The spec exists and the code does not.
[#2611](https://github.com/moq-dev/moq/pull/2611) added epochs and ended
broadcasts to the lite-06 draft, while
[#1920](https://github.com/moq-dev/moq/pull/1920) had earlier removed the
epoch that used to be in the wire. `AnnounceBroadcast::Active` carries
`{ suffix, hops, cost }` on both `main` and `dev`; `broadcast.rs`'s `route_epoch`
and `routes_epoch` are unrelated route-change counters and are not this.

Adding a field to ANNOUNCE_START and ANNOUNCE_UPDATE has the same skew hazard
[advertise](/quest/m2/wildcard/advertise.md) states for the wildcard message: Lite06 is one ALPN
with no sub-version, and an earlier Lite06 peer reads the extra varint as the
next message and kills the stream. Stage it the same way, decode tolerance
fleet-wide before any sender emits the field, and test the sender against the
intermediate accept-only build.

Implement what the draft already states: a generation on ANNOUNCE_START and
ANNOUNCE_UPDATE, forwarded unchanged by relays; the highest epoch wins and a
lower one never displaces it; equal non-zero epochs declare interchangeable
content that a relay MAY splice at a group boundary; anything else is two
generations colliding, which MUST NOT splice and MUST end the lower rather than
wait for it to drop. Zero falls back to the first hop entry as identity. The
draft's recommended construction (milliseconds shifted left 16 with random low
bits, clamped above the highest observable epoch) covers restarts without
persisted state.

For transcode workers the point is the interchangeable case: two workers
that both claim one path publish concrete announcements at the SAME literal
path (the contribution is published where it is addressed, so there is no
cross-root front to reconcile) and derive the SAME epoch from the source
generation, so the loser's viewers migrate at a group boundary rather than
being torn down.
That only holds because the transcode contract requires its redundant output to
be interchangeable. Session-local services with distinct ordered epochs are
also permitted; those replace the lower generation and never splice.
Test both generic Epoch behaviors rather than assuming either at the routing
layer.

Cover: a higher epoch displacing a lower, a lower one being ignored, equal
epochs splicing mid-stream without a visible gap, unequal epochs ending the
loser promptly, and the zero fallback.
