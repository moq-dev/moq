# Migrating hang

How to update your code for each breaking change, newest first.

## Unreleased

### `M2ts::random_access` covers the whole track (#5967)

`random_access` no longer describes the latest two-second window: it is true only while every group the track has published started at a random access point.
Once a group starts any other way (a fallback cut, a size cut, or a layout with several programs or video streams), it turns false for the rest of the track.
Don't wait for it to come back: treat false as final, and resubscribe to a new broadcast if you need random access again.

`mux_rate` is the rate the publisher measures over its own recent PCR intervals, not a window on a grid shared between publishers.
It can be absent until the first measurement, change when the source's rate does, and clear when the source stops holding one rate, so read it again on each catalog update.

### The catalog root carries a reader `version` (#5806)

`version` is reserved at the catalog root: decoding refuses a catalog newer than this crate reads with `Error::UnsupportedVersion`, and serializing fails if the extension `E` has its own `version` member.
Rename that member, such as to `app_version`.

### Archive timelines declare a per-track duration (#5715)

Each `Archive::timelines` value is a `Timeline` (the timeline `track` plus its nominal `duration` in the archive timescale, `None` when untimed) instead of the timeline track's name, and `Archive::duration_max` is removed.
A catalog on the other side of this change decodes with no archive; see the [upgrade guide](https://github.com/moq-dev/moq/blob/main/doc/setup/upgrade.md).
Insert `Timeline::new(track, duration)` and read the track name from `.track`.

### Timeline records are optionally timed (#5720)

`timeline::Record::pts` and `duration` are `Option<u64>`, absent on an untimed track.
Build an untimed record with `Record::untimed(sequence, start, end)`.
Handle `None` where you read `record.pts` or `record.duration`.

### One `enabled` flag replaces `stalled` (#4915)

`VideoConfig::stalled: Option<bool>` is `enabled: bool`, with the meaning inverted, and `AudioConfig` gains `enabled`.
The `catalog::stalled` module is gone, and `Video::ranked()` puts disabled renditions last.
`stalled` is no longer written and is ignored on read.
Set `enabled = false` where you set `stalled = Some(true)`.

## 0.20 -> 0.21

### The catalog's `timeline` entry is `archive`

The root catalog's `timeline` is `archive` (`catalog::Archive`), and wall time moved to the root `clock: { wall, timescale }`.
An old publisher's catalog has no `archive`, so upgrade publishers before readers that need it.
Read wall time from `catalog.clock` instead of `catalog.timeline`.
