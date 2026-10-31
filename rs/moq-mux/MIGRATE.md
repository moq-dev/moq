# Migrating moq-mux

How to update your code for each breaking change, newest first.

## Unreleased

### Compression moves to the track

`catalog.json.z` is gone: `catalog::Producer` publishes one `catalog.json` with `compress: true` (the MSF `catalog` stays uncompressed), and HLS record tracks are compressed too.
`CatalogFormat::HangZ` and `catalog::hang::Consumer::compressed` are removed; read `CatalogFormat::Hang` with `Consumer::new`, which reads a compressed track like a plain one.
`json::Config::compression` and `with_compression` are removed: create the track with `moq_net::track::Info::default().with_compress(true)` instead. `Error::UnsupportedCompression` and `Error::Flate` are gone with them.
The `binary` module and `catalog::Producer::binary_snapshot` and `binary_stream` are deleted, along with the moq-flate dependency: publish opaque bytes on a plain track, or JSON through `json_snapshot` and `json_stream`.

### A decoder identity change publishes a new track ([#7110](https://github.com/moq-dev/moq/pull/7110))

A rendition never changes its identity in place: `codec`, `description`, `container`, `display_aspect_width`/`display_aspect_height`, and `interlaced` for video, and `codec`, `description`, `container`, `sample_rate`, and `channel_count` for audio (see `hang::catalog::VideoConfig::same_identity`).
`container::Producer::set`, `replace`, and `stage` refuse such a change with `Error::IdentityChanged`; every other field still updates in place.
The codec importers (H.264, H.265, AV1, VP8, VP9) publish a stream whose identity changes, such as a new SPS profile or level, or a new avcC, under the next free `<name>/<n>` and finish the old track.
TS AAC, MP2, AC-3, and E-AC-3, and FLV in-band audio start a new track when a frame header changes the format instead of ignoring it.
Read `name()`, `demand()`, and `track()` from the importer after each write rather than caching them.
An av1C without a sequence header in its `configOBUs` no longer publishes a rendition: the stream's first sequence header does.

### `moq_net::Path` has no lifetime

`Source::resolve_reference` takes `Option<&Relative>` and returns `Option<Path>`, `ts::Programs::new` takes `impl Into<moq_net::Path>`, so pass a `Path`, and `follow::Instances::new` takes `impl TryInto<moq_net::Path, Error: Into<moq_net::Error>>`. Replace `PathOwned` with `Path` and drop `to_owned()` and `into_owned()` on paths. See moq-net's migration for the rest.

### MKV and FLV import refuse malformed tracks and unsupported codecs (#6908)

MKV import fails on a malformed track, a duplicate or zero `TrackNumber`, a zero `TimestampScale`, a block timestamp outside the `u64` timeline, an unsupported codec or subtitle encoding, or input truncated at `finish()`, instead of skipping the track or wrapping the timestamp. Input after `finish()` fails with `mkv::Error::TracksAfterFinish`.
FLV import fails with `Error::UnknownFormat` on an unsupported legacy or enhanced codec instead of dropping its tags.
Supply valid metadata and supported codecs, or transcode or remove unsupported media tracks before import. Tracks of an unsupported role (such as MKV buttons) are still skipped.

### Rate following can disable a simulcast rung

`rate::Follower::next` and `poll_next` return a `rate::Target`, and `rate::Control::update` returns `Option<rate::Target>`: `Target::Enabled(rate)` is the bitrate to encode at, and `Target::Disabled` means stop encoding and advertise the rendition disabled.
`rate::Policy` gains `lower`, the next lower rendition's ceiling, which only a rendition encoded beside lower ones sets; add `lower: None` to a struct literal, or build it with `Policy::new`.
Match `Target::Enabled(rate)` where you took the rate before. A policy without `lower` never yields `Target::Disabled`.

### Repeated MSF track names and PMT PIDs keep the last entry

`catalog::msf::Error::DuplicateTrackName` is gone. An MSF catalog naming a track twice keeps the later track as that rendition, and a hang catalog projected to MSF advertises one track per namespace and name, the later rendition's.
A TS PMT listing a PID twice routes it by the later entry instead of failing the import.
Delete matches on `DuplicateTrackName`.

### HLS records replace timelines

The `timeline` module is replaced by `hls`, and records are still published by default.
`catalog::Config::with_timeline(config)` is `with_hls(config)`, and `with_hls(None)` publishes none.
`catalog::Reserved::with_timeline_hint` is `with_hls_hint`, and `catalog::Producer::timeline()` is gone.
Each video and audio rendition advertises its own `hls` field and `<track>.hls` record track, cut at group boundaries; untimed and data tracks get none.
Read records with `hls::Consumer::subscribe(&broadcast, &hls)`, whose `Event::Push` carries an `hls::Entry`, or publish your own with `hls::Producer`.
`Config::duration_min` is `hls::Config::duration`, `with_duration_min` is `with_duration`, and `DEFAULT_DURATION_MIN` is `DEFAULT_DURATION`.
It is now the spacing of a pts grid rather than a floor: a group starts a segment when its pts falls in a later multiple of it than the previous group's, so a segment may be shorter.
Each rendition cuts and numbers its own segments, so they line up across renditions wherever keyframes share timestamps.
`container::fmp4::Muxer::fragment` takes a third argument, where the content ends, which times the last sample when neither its codec nor a duration marker does; pass `None` to keep the old behavior.

### JSON snapshot encoder options move to `json::Options`

`json::Config::deltas` and `with_deltas` move to the new `json::Options`, which `catalog::Producer::json_snapshot` takes as a third argument, so a custom catalog entry can set them too.
Pass `json::Options::default()` where you relied on the default, or `json::Options::default().with_deltas(false)` where you called `Config::with_deltas(false)`.

### `Programs::finish` returns the program broadcasts

`Programs::finish` now returns each program's `broadcast::Producer` instead of `()`.
Hand them, with the `Programs` that owns their tracks, to the new `moq_mux::Linger` to keep the finished programs up for a fixed time from the end:

```rust
let broadcasts = programs.finish()?;
moq_mux::Linger::new(broadcasts, programs, moq_mux::Linger::DEFAULT).wait().await;
```

### TS service information expires on the program clock

TS import retracts SI sub-tables and incomplete generations that stop repeating within their table's maximum interval (30 seconds for unknown tables).
Repeat current tables within that interval; stale tables no longer remain advertised indefinitely.
Host time and stalled PCR do not age SI, and existing readers can finish snapshots they already hold.

### Passthrough fMP4 refuses a rewound group as `TimestampRewind` (#6426)

A passthrough `container::fmp4::Import` refuses a group that starts below the previous one with `Error::TimestampRewind` before opening it, as an unwrapped track does, instead of `Error::Moq(moq_net::Error::TimestampMismatch)` from a group it already opened.
Match `TimestampRewind` where you matched `TimestampMismatch`.

### `Source` waits for a sibling's first announcement (#6254)

`Source::bind`, `resolve`, and `subscribe_track` now wait for a sibling nothing announces yet instead of failing with `Unroutable`, and keep the first instance they resolve: once it goes, they answer that ended broadcast rather than waiting for the next.
Where you skipped a rendition on that error, bound the call yourself (`tokio::time::timeout`, or drop it), or follow the sibling's instances with `Source::follow`.

### JSON `delta_ratio` is `deltas`

`json::Config::delta_ratio` and `with_delta_ratio` are replaced by `deltas: bool` and `with_deltas` (default `true`).
Call `with_deltas(false)` where you called `with_delta_ratio(0)`.
Snapshot groups roll on joiner demand: create the track with `Info::refresh` and drive `json::Snapshot::refresh` (or `poll_refresh`) beside your updates.

### Priorities are signed (#6142)

`track_info` on the catalog producer and tracks takes an `i8` priority, higher first, with 0 unset; pass `hang::catalog::PRIORITY` as before.

### fMP4 import unwraps to Legacy (#6250)

`container::fmp4::Import` now publishes one `Legacy` frame per sample instead of one `Cmaf` frame per fragment.
Call `.with_passthrough(true)` to keep publishing CMAF.
`import::ContainerFormat::Fmp4` follows, and `"cmaf"` no longer parses as its alias but as the new `ContainerFormat::Cmaf`, which keeps CMAF.

### `Container` decodes one frame instead of reading a group

`Container::poll_read(group, waiter)` and its async `read(group)` are replaced by `Container::decode(frame)`, which decodes one `moq_net::frame::Frame` into media frames.
Read the frame yourself and hand it over: `while let Some(frame) = group.read_frame().await? { for media in container.decode(frame)? { .. } }`.
An implementation drops its `group.poll_read_frame` call and its `None` end-of-group case, and decodes the frame it is given.

### `Source` resolution takes the reference's epoch (#5699)

`Source::bind(rel)`, `resolve(rel)`, and `subscribe_track(rel, name)` take the catalog entry's optional epoch after `rel`.
Pass `config.epoch.as_ref()` (or `Reference::epoch`), or `None` to accept the first instance resolved as before.
A broadcast proven to serve another epoch fails with the new `Error::EpochMismatch`; over a route without an epoch the check is best effort.

### `ts::stats::Export`'s release counters move into `release` (#5849)

`dropped`, `trimmed`, `gaps`, `drift`, and `out_of_tolerance` are now fields of `Export::release`, a `ts::stats::Release`, which `ts::Release::stats` also returns on its own.
Read `export.release.dropped` where you read `export.dropped`, and likewise for the rest.

### Catalog read errors use `hang`'s variants (#6114)

`catalog::hang::Consumer` reports a lost catalog track as `Error::Moq` instead of `Error::Json(moq_json::Error::Net(_))`, and any other catalog decode failure (such as a bad DEFLATE stream) as `Error::Hang(hang::Error::Json(_))` instead of `Error::Json(_)`.
Match `Error::Moq` to detect a lost source.

### JSON and binary consumers return `Timed<T>` (#5677)

The `json` and `binary` consumers' `next` / `poll_next` return `moq_net::Timed<T>`; read `.value` and the optional `.at`.
On a snapshot track `next` yields every state; call `latest` / `poll_latest` for the newest. `latest` on a stream track fails with `UnsupportedMode`.

### `ts::Passthrough` describes the whole track in its `m2ts` section (#5967)

The section's `randomAccess` turns false at the first group that does not start at a random access point and stays false, instead of recovering with the next two-second window; its `muxRate` follows the source's measured rate.
A consumer that waited for `randomAccess` to return after a fallback cut now waits forever: treat false as final for the track.
A source that never marks random access gets a group one second after each group's start, rather than on a one-second grid shared by every importer, and the first catalog names the track with its first group instead of after the first window.

### `catalog::Timebase::is_on` is removed (#5967)

Nothing needs to check which catalog a timebase came from any more: mint it from the catalog the importer writes, with `Producer::timebase` or `Producer::reserve`, and drop the call.

### TS sections use PCR placement

A `verbatim` section frame's timestamp is the program clock at its first section byte, derived from the PCR, rather than the latest video PTS.
Stamp hand-built section frames with their arrival on that clock, as for `pesPacket`; import existing recordings from their source again to recover the placement.
A PMT naming no PCR PID refuses section streams; a later PMT naming one restores their routing. Sections before the first PCR or past a lone PCR are dropped and counted as `unplaced`, as whole PES are.
SCTE-35 splice times still absorb the media's offset through `pts_adjustment`; the frame timestamp does not name the splice time.

### TS private PES rides `pesPacket` tracks; `verbatim` is sections only (#5921)

`ts::Framing::Pes` and `ts::Verbatim::stream_id` are gone, and `Verbatim::framing` no longer defaults: a catalog naming `"framing": "pes"`, or a `verbatim` record without `framing`, fails to parse.
`ts::Import` now publishes every undecoded PES stream as a `ts::PesPacket` track, each frame one whole PES packet placed by the PCR.
Write a hand-built PES track as `Track { pes_packet: Some(PesPacket::new(stream_type)), .. }` with each frame the whole PES, and a `verbatim` one only for sections, with `Framing::Section`.
Recordings published with `pes` framing no longer export; import their source again.

### `fmp4::Wire::new` refuses a track that isn't audio or video (#5815)

`container::fmp4::Wire::new` returns `Result<Wire>`, as `Wire::from_init` already did, and refuses any other handler instead of treating it as data.
Handle the error from `Wire::new`.

### The catalog consumer is opaque (#5630)

Construct `catalog::Consumer` with `Consumer::new(broadcast, format).await`, or convert a `catalog::hang::Consumer` with `.into()`, then read it through `catalog::Stream`.
Its `Hang`, `Msf`, and `Closed` variants and `catalog::msf::Consumer::track` are private.

### An unset host clock publishes no wall mapping (#5429)

`Clock::wall()` returns `Option<hang::catalog::Clock>` instead of panicking, so handle `None`.
`Clock::wall_clock` and `Timebase::place` refuse a clock without one.

### TS export lingers within one publisher instance (#5147)

`ts::Export::resume` is `ts::Export::follow`, `Source::returned` is gone, and `ts::Follower` follows a path's announcements the way `moq export ts` does.
`ts::Export::new(source, catalog)` takes a catalog stream like the other exporters: subscribe with `source.catalog::<ts::Ext>(format).await?` to keep MPEG-TS metadata, or `()` for media only, and select renditions on the stream before passing it in.
`follow(broadcast, catalog)` takes the replacement stream too, both methods are synchronous, and `with_ts` and `with_catalog_format` are removed.

### `Source` is built from the resolved broadcast (#5252)

`Source::new(origin, path)` is `Source::new(origin, broadcast)`: resolve the broadcast first, and catalog references resolve from its path.
`Source::broadcast()` returns it without awaiting.
Every later request for a path, siblings included, stays on the broadcast first held there and fails once it ends, so build a new `Source` to follow a new instance.

```rust
let broadcast = origin.request_broadcast(path, None).await?;
let source = Source::new(origin, broadcast);
```

### The export delay is `with_delay` (#4645)

`ts::Export::with_max_age` and the FLV and MKV exports' `with_max_age` are `with_delay`: each frame is written that long after its decode time, on a clock that follows the source's.
`ts::Export` defaults to 1 s, and the send-ahead comes out of the same budget, so the output trails a TS source by the delay instead of twice it.
`ts::stats::Export` gains a dropped-frame count, measured drift, and an out-of-tolerance count, and is no longer `Eq`.

### Subscriber staleness is `max_delay`

| Before | After |
| --- | --- |
| `container::Consumer::set_max_age` | `set_max_delay` |
| fMP4, H.264, and H.265 exports' `with_max_age` | `with_max_delay` |

### Importers keep the source's timestamps

`clock::Anchor`, `clock::Lane`, `SourceMap`, `Clock::source`, and the importers' `live()` are gone; delete the `live()` calls.
Importers publish the source's own timestamps and anchor the catalog clock on their first frame.
An importer whose first frame arrives once the clock is in use (taken with `catalog.clock()`, published, or pinned with `Config::with_clock`) shifts its timestamps onto it, so its first frame lands at now.
Importers sharing a timestamp base reserve through one `catalog.timebase()`.

### Importers refuse a timestamp rewind

A backward timestamp is an encoder restart and needs a new broadcast.
Every importer, MPEG-TS included, now ends with `TimestampRewind` on a loop wrap, restart, or join below the edge instead of shifting past it.
`container::Producer` refuses a group that does not start strictly after the previous group's start, or a frame below the start of the group before its own.

### Importers publish the catalog at their first frame

The fMP4, MKV, and MPEG-TS importers hold the catalog until the first frame of a selected track, as FLV already did, so the first snapshot carries the final root `clock`.
A track deselected by `with_select` does not release it.
A `moov` or `Tracks` decoded after `finish()` is refused with `fmp4::Error::MoovAfterFinish` or `mkv::Error::TracksAfterFinish`.

### Video importers drop `tick`, `idle`, and `observe_lag` (#4915)

The H.264, H.265, AV1, VP8, and VP9 `Import`s no longer take liveness hints; a stalled rendition is marked by the catalog's `enabled` flag instead.
Delete the calls.

### fMP4 export fixes its track set at the init segment

`fmp4::Error` drops `MissingVideoTrack`, `MissingAudioTrack`, and `NoCatalogSnapshot`, and adds `TrackAdded`, `TrackChanged`, `TrackRewound`, and `TrackUndescribed`.
`fmp4::Export` ends with one of these where it used to write a track missing from the moov, and a broadcast that ends with media queued behind an undescribed track is an error instead of `Ok(None)`.
Restart the export to pick up a new rendition.

### fMP4 export of Annex-B H.264 and H.265 starts from the catalog

When the catalog codec string and dimensions are enough, the export writes an `avc3` or `hev1` init before the first keyframe and leaves the parameter sets in the samples.
A keyframe whose parameter sets never appeared ends the export instead of waiting 30 seconds.

### CMAF timescales must match the init

CMAF `Wire::write` refuses a track whose timescale is not the init's `mdhd` timescale with `fmp4::Error::TimescaleMismatch`.
A track you create yourself declares it with `track::Info::with_timescale`, passed to `create_track`.

```rust
let info = track::Info::default().with_timescale(Timescale::new(mdhd_timescale)?);
let track = broadcast.create_track(name, info)?;
```

### Data timestamps use the broadcast clock (#4821)

JSON and flate producers publish `at: None` untimed, so their track must be untimed too; pass a timestamp on a timed track.
Convert a capture `Instant` through the catalog clock.

```rust
producer.update(Timed::from(&value).at(catalog.clock().capture(instant)?))?;
```

### moq-binary is moq-flate

`Error::Binary` is `Error::Flate`, carrying a `moq_flate::Error`.

### AAC refuses channel counts it cannot name (#4973)

`aac::Config::encode` returns a `Result` and fails with `UnsupportedChannelCount` for a count no `channelConfiguration` names, instead of writing stereo.
`From<aac::Config> for hang::catalog::AudioConfig` is `TryFrom`, so convert with `try_into()?`.

### The Opus mapping family lives only on `mapping` (#4851)

`codec::opus::Config::mapping_family` is gone; drop the field.
Family 0 is `mapping: None`; any other family is `mapping.family()`.

### TS stats live in `ts::stats` (#4909)

`ts::Stats` is `ts::stats::Snapshot`, and `ts::StreamStats` is `ts::stats::Stream`, whose `track` is an owned `String`.
`ts::Export::stats` returns `ts::stats::Export`; feed it to `stats::Log` with `.into()`.
`ts::MultipleProgramsError` is `#[non_exhaustive]`: recover it by downcast and read `programs`.

### FLV export takes a catalog stream (#4987)

`flv::Export::new` is synchronous and takes `(source, catalog)`, like `fmp4::Export` and `mkv::Export`.
`with_catalog_format` and `with_select` are gone: open the catalog yourself with `source.catalog(format).await?` and narrow it with `catalog::Stream::select`.

## 0.9 -> 0.10

### Catalog edits are fallible (#3644, #3813)

`lock()` is `modify()?`, and a guard that fails to publish on drop aborts the track.
`timeline::Config::wall` is the broadcast `Clock`.

### Imports are typed

`import::Init` splits into `AudioInit`, `VideoInit`, and `ContainerInit`, with typed formats instead of strings.
`Track::new` is `Track::audio` or `Track::video`.
