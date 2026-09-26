# [S] Cached groups expire on the IETF path

## Goal

With the default cache pool, a relay's memory plateaus once groups start
reaching the expiry window, on moq-transport as on moq-lite, or the cause of
continued growth is found and fixed.

## Plan

In `session_delivery_broadcasts` with 4096 announced broadcasts and the
default unbounded pool (30 s expiry), IETF RSS reached 1.5 GB at 10 s, 3.5 GB at
30 s, and 4.9 GB at 50 s, still climbing past the expiry window. Lite
reached about 0.36 GB by 50 s. With a 16 MiB bounded pool both stay flat, so
the retained bytes are cached groups, not a leak elsewhere.

Reproduce with a focused test first. Unexpired groups at higher throughput,
expiry not running on a path IETF uses, or groups held outside the pool's
accounting would each explain it. Fix what is actually wrong. Consider
whether an unbounded default is the right default for an origin at all.

## Related

- [Relay memory](/quest/m1/relay-memory.md) - what an announcement costs in memory
