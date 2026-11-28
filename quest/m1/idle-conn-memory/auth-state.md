# [S] A session's auth state stops allocating B-tree leaves for one entry

## Goal

The auth state an idle session holds drops from 9.9 KiB to under 4 KiB,
chiefly by not allocating a full B-tree leaf for a map that almost always
holds one entry.

## Plan

The 2026-11-27 idle profile ([epic](/quest/m1/idle-conn-memory/README.md))
found, per connection:

- `moq_net::auth::State::links: BTreeMap<u64, Attached>`
  (`rs/moq-net/src/auth.rs`): 3.5 KiB from `Link::attach`, plus 0.6 KiB from
  `Link::bound`. A `BTreeMap` allocates an 11-entry leaf for its first
  insert, and `Attached` is large, so one session's one link costs a 3.5 KiB
  leaf.
- `moq_relay::auth::refresh::Refresh::credentials: BTreeMap<u64, Credential>`
  (`rs/moq-relay/src/auth/refresh.rs`): 3.1 KiB the same way.
- `Refresh::arm_changes`' boxed watch future 0.8 KiB, the
  `moq_auth::lease::Consumer` channel 0.6 KiB, and `Lease::preparing`
  0.6 KiB.

Landed: both maps box their values (`BTreeMap<u64, Box<Attached>>`,
`BTreeMap<u64, Box<Credential>>`), so a leaf holds pointers and ordered
iteration (`live`, oldest-first links) is unchanged. A counting allocator
puts one entry at 3,360 B inline against 488 B boxed for `Attached` (296 B),
and 3,008 B against 456 B for `Credential` (264 B): 5.4 KiB saved per
connection. `Refresh::arm_changes`' boxed watch future also shrank from
528 B to 272 B by boxing its expiry timer, which a credential without an
expiry (the profiled `--auth-public ""` case) no longer reserves. That
leaves about 4.1 KiB in the layer.

Remaining:

- Confirm the saving with a heap profile once the epic's `idle` workload
  exists, since this was sized by allocation and `size_of`, not by `jeprof`.
- `Attached::issued` and `Attached::slots` are further `BTreeMap`s (`Grant`
  is 64 B). If an idle session fills either, its leaf is another 0.8 KiB or
  so; the profile will say, and boxing them is the same change.

## Waiting

- Maintainer: the boxing saved an estimated 5.4 KiB of the layer's 9.9 KiB, leaving about 4.5 KiB against the goal's 4 KiB, and the rest is single-allocation state a session needs. Finish this quest on the boxing, or keep it for the remainder?
  - Finish on the boxing (Recommended) - the target was an estimate, the remaining 0.5 KiB has no simple fix, and the epic's idle profile will show the real number
  - Keep the quest open - the non-expiring-credential future split and a heap-profile confirmation stay here
