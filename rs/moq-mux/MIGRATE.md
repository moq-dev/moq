# Migrating moq-mux

How to update your code for each breaking change, newest first.

## Unreleased

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

### `ts::Programs::finish` returns each program's `Finish` (#5795)

`Programs::finish` returns `anyhow::Result<Vec<moq_net::broadcast::Finish>>` instead of `anyhow::Result<()>`.
Each handle keeps its program's broadcast servable until its media is stale, and dropping it closes that program.
Await `Finish::wait` on each handle to linger, or drop the handles to close every program now.

### Timeline durations are hints (#5715)

`timeline::Config::duration_max`, `with_duration_max`, and `DEFAULT_DURATION_MAX` are gone; set `duration_hint` with `with_duration_hint`, the expected record duration such as the GOP, and the split ceiling follows from it.
`DEFAULT_DURATION_MIN` is 4 s instead of 2 s, so set `with_duration_min` to keep the shorter records.

### The catalog consumer is opaque (#5630)

Construct `catalog::Consumer` with `Consumer::new(broadcast, format).await`, or convert a `catalog::hang::Consumer` with `.into()`, then read it through `catalog::Stream`.
Its `Hang`, `Msf`, and `Closed` variants and `catalog::msf::Consumer::track` are private.

### Timelines record untimed tracks (#5720)

`timeline::Segmenter::frame` takes `pts: impl Into<Option<Timestamp>>`, so existing callers compile.
A segmenter fed the wrong timedness for its track fails with `Error::TimelineTiming` or `Error::TimelineUntimed`.

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
