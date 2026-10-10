# [L] Relay session benchmark through moq-relay

## Goal

The sans-IO session bench also runs through `moq-relay`'s own connection
handling, including auth, the cluster origin, and stats, over the in-memory
transport. Relay-layer costs then show up in benchmarks, not only the
`moq-net` model.

## Plan

Decided 2026-10-10: the seam (a generic-session `moq_relay::Connection` and
one in-memory session) is [the relay seam](/quest/m1/time/relay-seam.md),
shared with the relay's sim tests; this quest is only the bench.

Reuse the scenario, the publisher and subscriber sweeps, and the delivery
accounting from `rs/moq-net/benches/session.rs` so the two results line up, and
the difference is the relay layer.

Run the dash shape (`session_dash_*`) first. In the model bench, the production
point (34 peer nodes x 5 projects x 24 tracks) costs about 100 ms of one core
per 1 s stats tick, for the whole mesh. In production, moving that one session
off a relay saved 50-90% of a core on that relay alone (2026-09-25). The gap is
the relay layer or the transport, not the model.

## Required

- [The relay seam](/quest/m1/time/relay-seam.md) - the in-memory session and generic relay connection this runs over
