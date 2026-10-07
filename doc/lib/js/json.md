---
title: "@moq/json"
description: JSON over MoQ tracks, as snapshots, streams, or a sliding window
---

# @moq/json

[![npm](https://img.shields.io/npm/v/@moq/json)](https://www.npmjs.com/package/@moq/json)

JSON over [`@moq/net`](/lib/js/net) tracks, in three modes. Snapshot and stream
are the catalog modes on the [hang](/concept/hang#data-tracks) page. Window is
a third framing for a raw track: both ends opt into it, and a generic catalog
reader will not pick it up.

- **Snapshot**: lossy latest value, with RFC 7396 merge-patch deltas. A delta before any snapshot is an error.
- **Stream**: lossless append-log in a single group. A reader that falls behind fails the read rather than resuming mid-log.
- **Window**: a bounded run of records a reader can join at any point. Trimming a record is explicit, so a reader that was keeping up is not handed that record again when the publisher rolls a group.

Both sides choose the same compression, `"none"` or `"deflate"`. A value is
stamped when written, unless you pass its capture time.

A stream rides one group, so the whole log shares `@moq/net`'s group budget:
32 MiB of payload and 8192 records. An append that might not fit is refused
before it is encoded and leaves the log intact. Once the budget is spent,
start a new track. Any other failed append aborts the track, so readers see
the error rather than a log that looks complete.

The Rust twin is [`moq-json`](/lib/rs/moq-json).
