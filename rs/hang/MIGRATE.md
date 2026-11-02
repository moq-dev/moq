# Migrating hang

How to update your code for each breaking change, newest first.

## Unreleased

### Compression moves to the track

`catalog::Compression`, the `compression` field on `JsonConfig` and `BinaryConfig`, `Catalog::COMPRESSED_NAME`, and `catalog::Consumer::compressed` are gone.
A compressed track is one name with `compress: true` on its `moq_net::track::Info`, and moq-net hands every reader plain frames, so read `catalog.json` with `Consumer::new` whether or not it is compressed.
`Catalog::default_track_info()` sets `compress`, so a publisher using it compresses `catalog.json`; stop publishing `catalog.json.z`.
An old catalog's `"compression"` key on a data track lands in the entry's `extra`, and its frames are still app-level DEFLATE nothing here inflates.

### `moq_net::Path` has no lifetime

`catalog` reference fields, `Echo::path`, and `health::Observer` use the lifetime-free `moq_net::path::Relative` and `moq_net::Path`: replace `RelativeOwned` with `Relative` and `PathOwned` with `Path`. `Echo::new` takes `impl Into<Relative>`: pass `Relative::new(s)?` rather than a string. See moq-net's migration for the rest.

### The catalog rendition cap is removed

`catalog::MAX_RENDITIONS` and `Error::TooManyRenditions` are gone: a catalog with any number of renditions is accepted.
Delete matches on `TooManyRenditions`. An application that wants a limit counts `video.renditions`, `audio.renditions`, and `text.renditions` itself.

### HLS records replace the root archive

`catalog::Archive`, `catalog::Timeline`, `Catalog::archive`, and the `timeline` module are gone.
A video or audio rendition carries its own `hls: Option<catalog::Hls>` field, naming its record track and nominal segment duration.
Records are `hls::Record { sequence, group, pts, duration }` in milliseconds on a `<track>.hls` track (`hls::default_name`, `hls::SUFFIX`).
Build one with `Record::new(sequence, group, pts)`, adding `with_duration(0)` where content stops.
A catalog with an old root `archive` still parses, with the entry ignored.
`catalog::MOQ_EPOCH_UNIX_MILLIS` moves to the `clock` module, re-exported at the same path.

### `Catalog` parses through `FromStr` (#6471)

The inherent `Catalog::from_str` is replaced by `impl FromStr for Catalog`, with the same `Error`.
Call `json.parse::<Catalog>()`, or bring `std::str::FromStr` into scope to keep calling `Catalog::from_str`.

### Priorities are signed (#6142)

`catalog::Priorities` fields and `container::track_info`'s priority are `i8`, higher first, with 0 unset.
`PRIORITY` keeps its values (100, 90, 80, 60), which now sit above the unset default instead of below it, so their moq-lite bytes move from 100..60 to 228..188.

### `catalog::Reference` names its epoch (#5699)

Every rendition and data track config gains an optional `epoch` beside `broadcast`.
`Reference` has a new required `epoch()`: an implementor outside this crate returns the entry's epoch, or `None` when it names none.

### A lost catalog track is `Error::Moq` (#6114)

`catalog::Consumer::next` and `poll_next` return `Error::Moq` when the catalog track fails, instead of flattening it into `Error::Json`.
Match `Error::Moq` to tell a lost source from a malformed catalog.

### `M2ts::random_access` covers the whole track (#5967)

`random_access` no longer describes the latest two-second window: it is true only while every group the track has published started at a random access point.
Once a group starts any other way (a fallback cut, a size cut, or a layout with several programs or video streams), it turns false for the rest of the track.
Don't wait for it to come back: treat false as final, and resubscribe to a new broadcast if you need random access again.

`mux_rate` is the rate the publisher measures over its own recent PCR intervals, not a window on a grid shared between publishers.
It can be absent until the first measurement, change when the source's rate does, and clear when the source stops holding one rate, so read it again on each catalog update.

### The catalog root carries a reader `version` (#5806)

`version` is reserved at the catalog root: decoding refuses a catalog newer than this crate reads with `Error::UnsupportedVersion`, and serializing fails if the extension `E` has its own `version` member.
Rename that member, such as to `app_version`.

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
