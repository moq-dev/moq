# [S] An abandoned subscription setup releases its streams

## Goal

On lite-07, a subscription whose last reader leaves while SUBSCRIBE is still
opening or awaiting its answer releases its upstream TRACK and pending
SUBSCRIBE, so their credit can serve later requests.

## Plan

Decided 2026-11-05 (maintainer, /quest-plan): ✅ Keep the TRACK/SUBSCRIBE
handshake and document the minimum of four concurrent bidirectional streams.
AUTH and ANNOUNCE hold two slots for the session lifetime; TRACK is briefly
held alongside SUBSCRIBE. Three slots cannot establish the first subscription.
Rejected: metadata on SUBSCRIBE (overlaps other lite-07 work and saves no round
trip), and releasing TRACK early during a live setup.

The established-subscription regression
`a_subscription_past_the_credit_is_served_once_credit_returns` already passes
with four slots. Keep that budget. The three-slot reproduction did not show a
departure leak.

Local review identified remaining pending-setup cleanup: `ServeLoop::poll` in
`ServeMode::Establish` waits on establishment without observing lost demand,
while holding `HeldTrack`. Reproduce abandonment with at least four slots,
keeping the publisher and route live, then ensure both the TRACK hold and the
pending SUBSCRIBE registration are released when demand leaves. Preserve the
hold while demand remains. Cover Rust and its JS counterpart where applicable.

Decided 2026-11-06 (maintainer, /quest-unblock): ✅ Rethink the handoff
around explicit requester ownership until the first reader attaches. Replace
the demand-arrived heuristic so a requester disappearing after metadata
acceptance releases a blocked SUBSCRIBE and its TRACK hold. Rejected: another
heuristic patch, landing with the known gap, and dropping the PR.

Use the existing `track::Resolving` requester handle as ownership across
metadata acceptance. It already contributes demand until dropped; subscribe
before releasing it to transfer ownership without a gap. Setup cancellation
therefore observes current owners rather than remembering demand history.
