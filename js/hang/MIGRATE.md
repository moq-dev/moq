# Migrating @moq/hang

How to update your code for each breaking change, newest first.

## Unreleased

### The catalog root reserves `version` (#5806)

`version` is the oldest moq-hang draft that reads the catalog, and parsing throws `UnsupportedVersion` for one newer than this reader.
Rename an extension field that was called `version`.

```ts
// Before
const Schema = z.extend(RootSchema, { version: z.string() });
// After
const Schema = z.extend(RootSchema, { appVersion: z.string() });
```

### Archive timelines declare a per-track duration (#5715)

Each `archive.timelines` value is `{ track, duration }` instead of the timeline track's name, and `archive.durationMax` is removed.
`duration` is the nominal record duration in the archive timescale, absent for an untimed track.
A catalog on the other side of this change parses with no `archive`; see the [upgrade guide](https://github.com/moq-dev/moq/blob/main/doc/setup/upgrade.md).

```ts
// Before
const track = catalog.archive?.timelines.video;
const bound = catalog.archive?.durationMax;
// After
const track = catalog.archive?.timelines.video?.track;
const duration = catalog.archive?.timelines.video?.duration;
```

### Timeline records are optionally timed (#5720)

`Timeline.Record.pts` and `.duration` are optional, absent on an untimed track.

```ts
// Before
const pts: number = record.pts;
// After
if (record.pts === undefined) continue;
```

### Catalog ranking names its media kind

`Catalog.ranked(renditions)` is `Catalog.Video.ranked(renditions)`.
`Catalog.Audio.ranked(renditions)` orders audio by enabled state, bitrate, sample rate, channel count, and name.
Both put disabled renditions last.

```ts
// Before
const best = Catalog.ranked(catalog.video.renditions)[0];
// After
const best = Catalog.Video.ranked(catalog.video.renditions)[0];
```

### One `enabled` flag replaces `stalled` (#4915)

`Catalog.Stalled` is gone, `VideoConfig.stalled` is `enabled` with the meaning inverted, and `AudioConfig` gains `enabled`.

```ts
// Before
if (config.stalled) skip();
// After
if (config.enabled === false) skip();
```

### CMAF decodes at the frame timestamp (#4826)

A fragment's earliest sample presents at its moq-net frame timestamp; `tfdt` only orders the samples.
`Container.Format.decode` and `Cmaf.decodeDataSegment` take that timestamp, or `undefined` for an untimed frame, whose samples present at `tfdt`.
`Cmaf.decodeTimestamp` is gone.

```ts
// Before
const frames = format.decode(frame.payload);
// After
const frames = format.decode(frame.payload, frame.timestamp);
```

### Container consumers take `maxDelay`

`Container.Consumer`'s `maxAge` prop is `maxDelay`, and passing `maxAge` throws a `TypeError`.

```ts
// Before
new Container.Consumer(track, { maxAge });
// After
new Container.Consumer(track, { maxDelay });
```

### `cut` is `discontinuity` (#4141)

`Container.Legacy.Producer.cut(end?)` is `discontinuity(end?)`.

```ts
// Before
producer.cut(end);
// After
producer.discontinuity(end);
```

## 0.4 -> 0.5

### The catalog's `timeline` entry is `archive`

@moq/hang reads the root catalog's `archive` entry instead of `timeline`, and wall time from the root `clock`.

```ts
// Before
const index = catalog.timeline;
const wall = catalog.timeline?.wall;
// After
const index = catalog.archive;
const wall = catalog.clock?.wall;
```
