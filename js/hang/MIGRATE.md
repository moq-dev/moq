# Migrating @moq/hang

How to update your code for each breaking change, newest first.

## Unreleased

### The root `clock` is `media.clock`, and the catalog declares a join interval ([#7652](https://github.com/moq-dev/moq/pull/7652))

The catalog root gains an optional `media` section holding what applies to every audio and video track: `clock`, moved from the root, and `join`, whose `interval` is the longest GOP in whole milliseconds.
The `RootSchema` `clock` field is gone, and a root `clock` from an older publisher is ignored.
Read `catalog.media?.clock` instead of `catalog.clock`, and write `catalog.media = { clock }` instead of `catalog.clock = clock`.
A publisher that wants its broadcast exported as HLS or DASH declares `media.join.interval`, promising every audio and video track starts a group at least that often, and keeps it fixed for the broadcast. `Catalog.MediaSchema` and `Catalog.JoinSchema` validate the section; a malformed `join` reads as absent.

### Group sequences are `bigint` ([#7681](https://github.com/moq-dev/moq/pull/7681))

Following `@moq/net`, a `Container.Consumer` read's `group` is a `bigint`.
Compare group sequences as bigints, such as `read.group === 3n`.

### `Container.Consumer` takes a `delay` ([#7311](https://github.com/moq-dev/moq/pull/7311), [#7597](https://github.com/moq-dev/moq/pull/7597))

`ConsumerProps.maxDelay` is `delay: GetterInit<Moq.Track.Delay>`, where `@moq/net`'s `Track.Delay` is `{ min?: Milli | null; max?: Milli | null }` and an omitted field is zero. The consumer reads a `null` `min` as zero.
Replace `maxDelay: d` with `delay: { max: d }`, or derive it from a signal. `max: null` is no limit.
`min` is the playout delay: the consumer starts at the group covering the live edge less `min` plus any `warmup`, waiting for it when the newest group arrives first. Before the first read, a backlog that already landed starts at the first group whose first frame is within `max`, plus the warmup, of the newest content, keeping the group covering the live edge less `min` and the warmup.

### The catalog rendition cap is removed

`Catalog.MAX_RENDITIONS` and `Catalog.TooManyRenditions` are gone: `checkRenditions`, `encode`, and `watch` accept any number of renditions.
Delete `instanceof TooManyRenditions` checks. An application that wants a limit counts the `renditions` of each section itself.

### The root archive is gone

`Catalog.ArchiveSchema`, `Catalog.ARCHIVE_VERSION`, the root `archive` field, and the `Timeline` module are gone, with no record track replacing them.
An HLS or DASH export reads the media tracks directly, numbering segments on the catalog's `media.join.interval` grid.
`Catalog.MOQ_EPOCH_UNIX_MILLIS` is unchanged.

### Container reads are a tagged union

`Container.Consumer.next()` returns a `Container.Read` union instead of an object with optional `frame` and continuity counters.
Switch on `kind`: `frame` carries `frame` and `group`, `gap` carries `timestamp` and `duration`, and `end` carries `group` and `end`.
Reset decoder state on `gap`, and process `end` as the declared media end rather than testing `continuous` or `discontinuity`.

### CMAF decoding is a separate module

Replace `Container.Cmaf` with `import * as Cmaf from "@moq/hang/cmaf"`.
Its `Format`, `decodeInitSegment`, and `decodeDataSegment` remain synchronous.
The CMAF encoder (`createVideoInitSegment`, `createAudioInitSegment`, and `encodeDataSegment`) is removed; Hang publishers write Legacy or LOC instead.

### A container decode error rejects `Consumer.next()` (#5942)

A payload the format fails to decode, or a data track's marker sharing its group, now rejects `Container.Consumer.next()` with that error and ends the reader.
It used to truncate the group, log the error, and read the lost tail as a gap.
Handle the rejection as fatal; only a reset or evicted group still reads as a gap.

### The catalog root reserves `version` (#5806)

`version` is the oldest moq-hang draft that reads the catalog, and parsing throws `UnsupportedVersion` for one newer than this reader.
Rename an extension field that was called `version`.

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
