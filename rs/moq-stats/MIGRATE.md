# Migrating moq-stats

How to update your code for each breaking change, newest first.

## Unreleased

### `Refusal` is `RefusalKind` (#6510)

The re-exported `moq_stats::Refusal` follows moq-net's rename: use `moq_stats::RefusalKind`.

### Track names and readers take an `Encoding` (#6472)

`traffic_track`, `sessions_track`, `totals_track`, `presence_track`, and `prefix_track` take an `Encoding` instead of a `compressed: bool`, and `consume::Config` and `aggregate::Config` replace `compression: bool` and `with_compression(bool)` with `encoding: Encoding` and `with_encoding(Encoding)`.
Pass `Encoding::Plain` for `false` and `Encoding::Compressed` for `true`.

The single-broadcast readers are named like the aggregate ones: `consume::Traffic` is now `consume::TrafficConsumer` and `consume::Sessions` is `consume::SessionsConsumer`.

### The `.z` flavor ends with the plain one past 32 MB

A frame whose uncompressed JSON passes the 32 MB group budget now ends both the plain track and its `.z` sibling with `FrameTooLarge`: a reader inflates the whole group, so compression no longer carries a larger frame.
A reader that fell back to `.z` for very large frames sees the same end the plain reader does.

### A returning group restarts under a new epoch (#4904)

A stats group that returns announces a new epoch and starts its group numbers at zero, instead of continuing the old sequence.
A reader that keyed history on the path alone should end that history when the announcement ends and start fresh on the next one.
