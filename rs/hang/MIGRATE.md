# Migrating hang

How to update your code for each breaking change, newest first.

## Unreleased

### The catalog root carries a reader `version` (#5806)

`version` is reserved at the catalog root: decoding refuses a catalog newer than this crate reads with `Error::UnsupportedVersion`, and serializing fails if the extension `E` has its own `version` member.
Rename that member.

```rust
// Before
#[derive(Serialize, Deserialize)]
struct Extra { version: u32 }
// After
#[derive(Serialize, Deserialize)]
struct Extra { app_version: u32 }
```

### Archive timelines declare a per-track duration (#5715)

Each `Archive::timelines` value is a `Timeline` (the timeline `track` plus its nominal `duration` in the archive timescale, `None` when untimed) instead of the timeline track's name, and `Archive::duration_max` is removed.
A catalog on the other side of this change decodes with no archive; see the [upgrade guide](https://github.com/moq-dev/moq/blob/main/doc/setup/upgrade.md).

```rust
// Before
archive.timelines.insert("video".to_string(), "video.timeline.z".to_string());
let track: &String = &archive.timelines["video"];
// After
archive.timelines.insert("video".to_string(), Timeline::new("video.timeline.z", 4_000));
let track: &String = &archive.timelines["video"].track;
```

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
