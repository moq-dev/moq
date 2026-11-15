---
title: moq-json
description: JSON over MoQ tracks, as snapshots, streams, or a sliding window
---

# moq-json

[![crates.io](https://img.shields.io/crates/v/moq-json)](https://crates.io/crates/moq-json)
[![docs.rs](https://docs.rs/moq-json/badge.svg)](https://docs.rs/moq-json)

JSON over [`moq-net`](/lib/rs/moq-net) tracks, in three modes. Snapshot and
stream are the catalog modes on the [hang](/concept/hang#data-tracks) page.
Window is a third framing for a raw track: both ends opt into it, and a
generic catalog reader will not pick it up.

The `poll_*` methods work by default. Enable the `async` Cargo feature to await
`next()`, `latest()`, or `refresh()` instead.

- **snapshot**: lossy latest value, with RFC 7396 merge-patch deltas. An unchanged update publishes nothing. Several owners can edit their own keys in place instead of clobbering one document. Deltas ride one group until a joiner wants a fresh one: set the track's `refresh` ratio (8 is typical) and drive `Producer::refresh`, which restates the value in a new group. Without a ratio a group rolls only at its caps.
- **stream**: lossless append-log in a single group. A reader that falls behind fails the read rather than resuming mid-log.
- **window**: a bounded run of records a reader can join at any point. A new group restates what it keeps explicitly, so a reader that was keeping up is not handed a record twice. Its ops roll on the track's `refresh` ratio the same way.

Compression is the track's: create it with `Info::default().with_compression(Compression::Deflate)`,
which JSON benefits from, and readers see plain frames either way. Every write is a
`moq_json::Timed::new(&value, at)`, and `at` must match the track. On a timed track it is the
capture time. An untimed track has no timescale, so there `at` is `Timestamp::unscaled(key)`:
a write that opens a group starts it at that key, which must pass every group the track holds,
and a delta inside the open group carries that group's key. Editing guards take the same
timestamp when opened, including when they publish on drop.

Consumers return `Timed<T>`: read `.value` for the decoded value and `.at` for
its frame timestamp, with the track's exact scale preserved (an unscaled group key on an
untimed track). Snapshot
`next()` yields states in order, skipping every group a newer one supersedes:
`snapshot::Consumer::new` takes the track and subscribes to it itself at a zero
`delay.max`, so nothing can raise its budget. Use `latest()` to skip to the
newest buffered state. Skipped states
are gone, and a later `next()` continues after them.

Upgrading from an older release: see moq-json's
[MIGRATE.md](https://github.com/moq-dev/moq/blob/main/rs/moq-json/MIGRATE.md).

A stream rides one group, which grows for the track's life. The cache keeps its
newest 32 MiB, less under memory pressure, and sheds older records, so a reader
that falls further behind, or joins once the first record was shed, fails with
`Evicted`. A record over 32 MiB is refused with `FrameTooLarge` and leaves the log
intact.

The TypeScript twin is [`@moq/json`](/lib/js/json). API:
[docs.rs/moq-json](https://docs.rs/moq-json).
