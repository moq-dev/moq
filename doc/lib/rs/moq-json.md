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

- **snapshot**: lossy latest value, with RFC 7396 merge-patch deltas. An unchanged update publishes nothing. Several owners can edit their own keys in place instead of clobbering one document. Deltas ride one group until a joiner wants a fresh one: set the track's `refresh` ratio (8 is typical) and drive `Producer::refresh`, which restates the value in a new group. Without a ratio a group rolls only at its caps.
- **stream**: lossless append-log in a single group. A reader that falls behind fails the read rather than resuming mid-log.
- **window**: a bounded run of records a reader can join at any point. A new group restates what it keeps explicitly, so a reader that was keeping up is not handed a record twice. Its ops roll on the track's `refresh` ratio the same way.

Compression is the track's: create it with `Info::default().with_compress(true)`,
which JSON benefits from, and readers see plain frames either way. A bare value stays untimed; publish
`moq_net::Timed::from(&value).at(captured)` to retain a capture timestamp.
The timestamp must match the track: an untimed track has no timescale, and a
timed track requires a timestamp on every write. Editing guards take the
same optional capture time when opened, including when they publish on drop.

Consumers return `Timed<T>`: read `.value` for the decoded value and `.at` for
its optional frame timestamp, with the track's exact scale preserved. Snapshot
`next()` yields states in order inside the subscription's `max_delay` budget;
set that budget when following a playhead behind live. Use `latest()` to skip
to the newest buffered state. Skipped states are gone, and a later `next()`
continues after them.

Upgrading from an older release: see moq-json's
[MIGRATE.md](https://github.com/moq-dev/moq/blob/main/rs/moq-json/MIGRATE.md).

A stream rides one group, so the whole log shares moq-net's group budget:
32 MiB of payload and 8192 records. An append that might not fit is refused
before anything is written and leaves the log intact. Once the budget is spent,
start a new track.

The TypeScript twin is [`@moq/json`](/lib/js/json). API:
[docs.rs/moq-json](https://docs.rs/moq-json).
