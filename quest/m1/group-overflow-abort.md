# [M] Abort an oversized open group instead of shedding its head

## Goal

An open group that outgrows its cache budget errors for every reader instead
of evicting frames from its front. One consistent failure replaces today's
split, where a reader that kept up streams the whole group while a late or new
one gets `Lagged`, and the head-shedding machinery becomes dead code to
delete. Decide first: this is a semantics change to the model in both
languages, not a bug fix.

## Plan

Today `rs/moq-net` (`model/group.rs`, `MAX_CACHE_BYTES`) and `js/net`
(`group.ts`, `MAX_GROUP_CACHE_BYTES` plus `MAX_GROUP_FRAMES`) evict from the
front of an open group once it passes the cap, and a reader positioned below
the eviction fails with `Lagged`. Readers at or above it keep going, which the
IETF publishers use to serve a draft-20 filter whose range excludes the
evicted prefix (`group::Consumer::skip_to` in Rust, `Group.ReadOptions.from`
in JS).

Aborting the group at the cap instead:

- keeps the memory bound, and the group also stops growing,
- gives every reader the same terminal error at the same point, instead of
  punishing only whoever subscribed late,
- deletes the per-reader eviction floors and the eviction bookkeeping
  (`offset` in Rust, `start`/`evicted` in JS) outright,
- costs little in practice: a group near 32 MiB is pathological (hang groups
  are GOP-sized), and shedding already denies the whole group to every new
  subscriber anyway.

Open questions to settle before implementing:

- The abort error code: `Lagged` blames the reader for the writer's overrun;
  the existing oversized-single-frame rejection (a write error) is the closer
  precedent, and the writer should learn about it too.
- Whether the JS frame-count cap (1024) aborts as well or simply goes away;
  Rust has only the byte budget.
- Pool-level eviction of whole idle groups is untouched; only per-group
  front-shedding changes.

## Related

- [#3123](/quest/m0/3123-moq-bench-a-lagged-group-permanently-ends-the.md) - how a subscriber should react to a lagged group
- [Group charge](/quest/m0/group-charge.md) - pool-level budget accounting, unaffected by this change
- [#3161](/quest/m1/3161-retention-should-reclaim-idle-open-groups-now-that-expiry.md) - open-group lifecycle work in the same area
