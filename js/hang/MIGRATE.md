# Migrating @moq/hang

How to update your code for each breaking change, newest first.

## Unreleased

### The `ietf-loc` container kind

`Catalog.Container` gains `{ kind: "ietf-loc" }`, draft-ietf-moq-loc-04's own framing: the properties are the moq-transport Object Properties and the payload is the bare codec bitstream. The existing `loc` kind is unchanged.
A `switch` over `container.kind` that is exhaustive needs a case for it; decode it with `Container.Loc.ObjectFormat`, or skip the rendition.
`Container.Format.decode` takes the frame's Object Properties as an optional third argument, and a decoded `Frame` may carry the codec `config` that replaces the catalog's `description`. A format that ignores both needs no change.

### The catalog rendition cap is removed

`Catalog.MAX_RENDITIONS` and `Catalog.TooManyRenditions` are gone: `checkRenditions`, `encode`, and `watch` accept any number of renditions.
Delete `instanceof TooManyRenditions` checks. An application that wants a limit counts the `renditions` of each section itself.

### HLS records replace the root archive

`Catalog.ArchiveSchema`, `Catalog.ARCHIVE_VERSION`, the root `archive` field, and the `Timeline` module are gone.
A video or audio rendition carries an optional `hls: { track, duration }` field instead; read its records with `Hls.Consumer.subscribe(broadcast, rendition.hls)`.
Each `Hls.Entry` is `{ sequence, group, pts, duration? }`, with `pts` and `duration` in microseconds; a zero `duration` marks where content stopped.
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
