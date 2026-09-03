# [L] Warm advertise

## Goal

A relay carrying a broadcast advertises that broadcast's exact path as its own
route, priced warm, so later subscribers reach the copy the cluster already has
instead of opening another one. It retracts that route when the broadcast goes
idle or its own path starts draining.

## Plan

Warmth is per-broadcast and a route covers a prefix, so the discount cannot ride
the prefix route a relay already advertises: carrying `pid/foo.hang` says
nothing about the rest of `pid/`. Advertise the exact path instead, as a second,
more specific route. `best_server` already filters to the longest covering
prefix before ordering by cost, so the warm route wins for that one broadcast
and changes nothing for its neighbors.

Price it the way the deleted discount did: warm zero (the ingress is already
paid for), cold forwarded accumulated, since cold prices the path this relay
would have to open if it were not already carrying. Two relays both carrying
then tie on warm and are separated by cold, which is what
[Rank](/quest/m2/pop-skipping/rank.md) builds on.

Lifecycle follows the state that already exists rather than a new timer.
The route appears when a track under the broadcast has demand and stays while
any track retains its source copy inside `TRACK_IDLE_LINGER`; it is retracted
once the last copy expires. `COST_LINGER` was the parallel five-second timer
with a different definition and is already gone with
[moq#3225](https://github.com/moq-dev/moq/pull/3225); do not reintroduce it.

Retract rather than reprice when the serving path drains. Under
specificity-first selection a ceiling-priced exact-path route still outranks
every broader route, so a drain that only repriced would keep attracting the
subscribers it is trying to move. Dropping the claim lets the broader route the
content is still reachable through take over. Update
`drafts/draft-lcurley-moq-lite.md` in the same change: this replaces the
ceiling-exemption paragraph that
[moq#3278](https://github.com/moq-dev/moq/pull/3278) removed, and it is a behavior change the
draft has to carry.

Make both Lite and IETF publishers advertise from one warmth signal, so a
cluster selecting Lite06 and one on the Cluster extension mean the same thing by
warm.

Tests: two tracks with staggered demand keep one warm route alive; demand
returning during retention does not churn the advertisement; the route is
retracted after the last copy expires and is not re-advertised afterwards; a
draining path retracts rather than repricing, and a subscriber on it moves to
the broader route; a second relay carrying the same broadcast ties on warm and
is separated by cold.
