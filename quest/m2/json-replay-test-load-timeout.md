# [XS] The JSON stream eviction test passes under a loaded host

## Goal

`js/json/src/stream/stream.test.ts`, "a live stream passes several tails and
refuses a truncated replay (compress=true)", passes when `just check` runs it
beside every other JS package. Today it appends 100 records of 1 MiB padding
through deflate, takes about 1s alone, and timed out at bun's 5s default
under `just check`'s parallel load (6.8s), while passing 5 of 5 runs alone.

## Plan

Shrink the work rather than raise the timeout: the eviction it checks needs
only enough bytes to pass the cache budget, so lower the padding or the
budget the test track uses until the case costs a fraction of a second.
