# Migrating hang

How to update your code for each breaking change, newest first.

## Unreleased

### Timeline records are optionally timed (#5720)

`timeline::Record::pts` and `duration` are `Option<u64>`, absent on an untimed track.
Build an untimed record with `Record::untimed(sequence, start, end)`.

```rust
// Before
let pts: u64 = record.pts;
// After
let Some(pts) = record.pts else { continue };
```

### One `enabled` flag replaces `stalled` (#4915)

`VideoConfig::stalled: Option<bool>` is `enabled: bool`, with the meaning inverted, and `AudioConfig` gains `enabled`.
The `catalog::stalled` module is gone, and `Video::ranked()` puts disabled renditions last.
`stalled` is no longer written and is ignored on read.

```rust
// Before
video.stalled = Some(true);
// After
video.enabled = false;
```

## 0.20 -> 0.21

### The catalog's `timeline` entry is `archive`

The root catalog's `timeline` is `archive` (`catalog::Archive`), and wall time moved to the root `clock: { wall, timescale }`.
An old publisher's catalog has no `archive`, so upgrade publishers before readers that need it.

```rust
// Before
let wall = catalog.timeline.and_then(|t| t.wall);
// After
let clock = catalog.clock;
```
