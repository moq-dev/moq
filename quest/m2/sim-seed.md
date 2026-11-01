# [S] Seeded sim

## Goal

Under the `kio::clock` sim, every source of randomness that affects behavior
(moq-net's clock anchor jitter, backoff jitter, alert jitter) draws from one
seeded generator the sim owns, and a failing sim test prints its seed so it
replays bit-exact.

## Plan

Decided 2026-10-10: with controlled time a test advances past the largest
jitter, so seeding only adds exact replay. Production keeps its OS randomness.
