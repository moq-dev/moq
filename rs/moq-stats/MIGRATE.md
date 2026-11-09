# Migrating moq-stats

How to update your code for each breaking change, newest first.

## Unreleased

### Async wrappers are opt-in

Enable the `async` Cargo feature to keep awaiting the readers' `next` and `Publisher::run`:
`moq-stats = { version = "...", features = ["async"] }`. This also applies with
`default-features = false`. The feature is off by default; without it, drive `poll_next` and
`Publisher::poll_run`.

`Consumer::{totals, presence, prefix, group}` return their reader at once instead of awaiting
the subscription handshake: drop the `.await`. The handshake finishes on the reader's first
`next`, which fails with its error, such as `NotFound` for an unlisted group.
The stats tracks and their JSON are unchanged.

### Group counters have their own epoch type

`GroupFrame::epoch` is `moq_stats::Epoch` instead of `moq_net::Instance`.
Use `Epoch` for stored group epochs; parse their text with `str::parse` or
`Epoch::try_from(String)`, and read it with `as_str` or `Display`.
Epochs are opaque identities with equality and hashing, without a temporal order.
Broadcast routes still use `moq_net::Instance`. Group JSON is unchanged:
producers mint UUIDv7 strings, and readers accept any nonempty epoch string.

### `moq_net::Path` has no lifetime

Functions that took `impl AsPath` take `impl Into<Path>`, so pass a `Path` rather than a string, and fields and returns that were `PathOwned` are `moq_net::Path`. See moq-net's migration for the rest.

### `Refusal` is `RefusalKind` (#6510)

The re-exported `moq_stats::Refusal` follows moq-net's rename: use `moq_stats::RefusalKind`.

### One compressed track per stats name

The `.z` siblings are gone: `totals.json`, `<label>/presence.json`, and `<path>/prefix.json` are each one track with `Compression::Deflate`, carrying merge-patch deltas, and moq-net hands every reader plain frames.
`totals_track`, `presence_track`, and `prefix_track` drop their `compressed: bool` argument, and `consume::Config` and `aggregate::Config` drop `compression` and `with_compression`; `consume::Config` is now empty.
`COMPRESSED_SUFFIX` is removed. Read the plain name whichever flavor you read before.
A frame whose uncompressed JSON passes the 32 MB group budget ends the track with `FrameTooLarge`.

The single-broadcast readers are named like the aggregate ones: `consume::Traffic` is now `consume::TrafficConsumer` and `consume::Sessions` is `consume::SessionsConsumer`.

### A returning group restarts under a new instance (#4904)

A stats group that returns announces a new instance and starts its group numbers at zero, instead of continuing the old sequence.
A reader that keyed history on the path alone should end that history when the announcement ends and start fresh on the next one.
