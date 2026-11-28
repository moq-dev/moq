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

Box the values so a leaf holds pointers, or keep the first entry inline
(a `Vec` or small-vector keyed by id), whichever keeps the lookups these
maps serve simple. Check whether the boxed watch future can be smaller once
the map change lands.
