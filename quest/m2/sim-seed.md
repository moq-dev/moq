# [S] Seeded sim

## Goal

Under the `moq-time` sim, every source of randomness that affects behavior
(moq-net's clock anchor jitter, backoff jitter, alert jitter) draws from one
seeded generator the sim owns, and a failing sim test prints its seed so it
replays bit-exact.

## Plan

Decided 2026-10-10: out of [controlled time](/quest/m1/time/README.md); with
controlled time a test advances past the largest jitter, so seeding only adds
exact replay. Production keeps its OS randomness.

## Required

- [moq-net on moq-time](/quest/m1/time/net.md) - the sim and its first jitter consumer
