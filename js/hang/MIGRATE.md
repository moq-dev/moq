# Migrating @moq/hang

How to update your code for each breaking change, newest first.

## Unreleased

### A container decode error rejects `Consumer.next()` (#5942)

A payload the format fails to decode, or a data track's marker sharing its group, now rejects `Container.Consumer.next()` with that error and ends the reader.
It used to truncate the group, log the error, and read the lost tail as a gap.
Handle the rejection as fatal; only a reset or evicted group still reads as a gap.

### The catalog root reserves `version` (#5806)

`version` is the oldest moq-hang draft that reads the catalog, and parsing throws `UnsupportedVersion` for one newer than this reader.
Rename an extension field that was called `version`.

### Archive timelines declare a per-track duration (#5715)

Each `archive.timelines` value is `{ track, duration }` instead of the timeline track's name, and `archive.durationMax` is removed.
`duration` is the nominal record duration in the archive timescale, absent for an untimed track.
Read the track name from the value's `track`, and use the per-track `duration` instead of `durationMax`.
A catalog on the other side of this change parses with no `archive`; see the [upgrade guide](https://github.com/moq-dev/moq/blob/main/doc/setup/upgrade.md).

### Timeline records are optionally timed (#5720)

`Timeline.Record.pts` and `.duration` are optional, absent on an untimed track.
Handle a record whose `pts` is `undefined`.

### Catalog ranking names its media kind

`Catalog.ranked(renditions)` is `Catalog.Video.ranked(renditions)`.
`Catalog.Audio.ranked(renditions)` orders audio by enabled state, bitrate, sample rate, channel count, and name.
Both put disabled renditions last.

### One `enabled` flag replaces `stalled` (#4915)

`Catalog.Stalled` is gone, `VideoConfig.stalled` is `enabled` with the meaning inverted, and `AudioConfig` gains `enabled`.
Replace a `config.stalled` check with `config.enabled === false`.

### CMAF decodes at the frame timestamp (#4826)

A fragment's earliest sample presents at its moq-net frame timestamp; `tfdt` only orders the samples.
`Container.Format.decode` and `Cmaf.decodeDataSegment` take that timestamp, or `undefined` for an untimed frame, whose samples present at `tfdt`.
Pass `frame.timestamp` after the payload.
`Cmaf.decodeTimestamp` is gone.

### Container consumers take `maxDelay`

`Container.Consumer`'s `maxAge` prop is `maxDelay`, and passing `maxAge` throws a `TypeError`.

### `cut` is `discontinuity` (#4141)

`Container.Legacy.Producer.cut(end?)` is `discontinuity(end?)`.

## 0.4 -> 0.5

### The catalog's `timeline` entry is `archive`

@moq/hang reads the root catalog's `archive` entry instead of `timeline`, and wall time from the root `clock`.
Read `catalog.archive` instead of `catalog.timeline`, and `catalog.clock?.wall` instead of `catalog.timeline?.wall`.
