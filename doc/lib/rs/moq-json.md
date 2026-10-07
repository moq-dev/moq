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

- **snapshot**: lossy latest value, with RFC 7396 merge-patch deltas. An unchanged update publishes nothing. Several owners can edit their own keys in place instead of clobbering one document.
- **stream**: lossless append-log in a single group. A reader that falls behind fails the read rather than resuming mid-log.
- **window**: a bounded run of records a reader can join at any point. A new group restates what it keeps explicitly, so a reader that was keeping up is not handed a record twice.

Both sides choose the same compression. A value is stamped when written,
unless it carries its capture time.

A stream rides one group, so the whole log shares moq-net's group budget:
32 MiB of payload and 8192 records. An append that might not fit is refused
before it is encoded and leaves the log intact. Once the budget is spent,
start a new track.

The TypeScript twin is [`@moq/json`](/lib/js/json). API:
[docs.rs/moq-json](https://docs.rs/moq-json).
