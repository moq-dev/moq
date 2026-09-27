# [S] Cached groups expire under the default pool

## Goal

With the default cache pool, a relay's memory plateaus once groups start
reaching the expiry window, on every version, or the cause of continued
growth is found and fixed.

## Plan

In `session_delivery_broadcasts` with 4096 announced broadcasts and the
default unbounded pool (30 s expiry), RSS keeps climbing past the expiry
window on both wires (2026-09-25, Apple M4, sampled every 10 s): IETF
1.9 GB at 30 s and 4.4 GB at 120 s, lite-06 2.3 GB at 30 s and 3.5 GB at
120 s, both oscillating by a gigabyte between samples. With a 16 MiB bounded
pool both stay flat (0.5 GB IETF, 0.4 GB lite), so the retained bytes are
cached groups, not a leak elsewhere. An earlier run saw growth on IETF only,
but lite was then 30x slower per round, so it wrote far fewer groups; compare
by groups written, not by wall time.

Reproduce with a focused test first. Unexpired groups at higher throughput,
expiry not running on some path, or groups held outside the pool's accounting
would each explain it. Fix what is actually wrong. Consider whether an
unbounded default is the right default for an origin at all.

## Related

- [Relay memory](/quest/m1/relay-memory.md) - what an announcement costs in memory
