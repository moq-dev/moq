# Migrating moq-hls

How to update your code for each breaking change, newest first.

## Unreleased

### Routes parse without the server, and media takes the parsed id

`server::Route` and `server::Resource` are `export::Route` and `export::Resource`, available without the `server` feature.
`Resource::Segment` and `Resource::SegmentAt` are one `Resource::Segment { kind, rendition, segment }`, whose `segments::Id` carries the generation, the reference tag, and `segments::At::Sequence` or `At::Pts`.
`Rendition::segment(sequence)` is `segment(&id)`, and refuses a segment URL from another generation.
`Rendition::init()` is `init(hash)`, answering only the init `Resource::Init` names.
Parse the request path with `Route::parse` instead of splitting it yourself, so an encoded `/` cannot smuggle a broadcast past your policy check, and pass the parsed id through:

```rust
let Some(route) = moq_hls::export::Route::parse(path) else { return not_found() };
match route.resource {
    Resource::Init { hash, .. } => rendition.init(&hash).await,
    Resource::Segment { segment, .. } => rendition.segment(&segment).await,
    _ => return not_found(),
}
```

To warm the init before a media playlist, call `Rendition::playlist(query)`, which builds it.

### The master playlist renderer is private

`export::master` is private. Render the multivariant playlist with `Broadcaster::master_playlist`.

### A failed timeline fails the segment cursor

`export::segments::Consumer::next` returns `Err(Error::TimelineFailed { .. })` once a failed timeline's covered segments drain, instead of `Ok(None)`.
`Ok(None)` now means only a clean end, so finalize a recording there, and mark it truncated on the error.

```rust
match cursor.next().await {
	Ok(Some(segment)) => { /* store it */ }
	Ok(None) => { /* complete: finalize the recording */ }
	Err(Error::TimelineFailed { .. }) => { /* truncated: every later call fails the same */ }
	Err(err) => { /* a fetch failed: call next() again to retry it */ }
}
```

### HLS import unwraps fMP4 to Legacy (#6250)

`import::Import` now publishes one `Legacy` frame per sample instead of one `Cmaf` frame per fragment.
Set `import::Config::passthrough` to keep publishing CMAF.

### An ended playlist ends the import

`import::Import::run` returns `Ok(())` once every rendition's playlist carries `EXT-X-ENDLIST` and its last segment is imported, instead of polling forever.
A playlist that has already ended (`EXT-X-ENDLIST`) when a rendition first sees it is refused with the new `Error::PlaylistEnded`, from `init` and `run` alike, instead of importing its last three segments.
Call `Import::finish` then, and keep the returned `broadcast::Finish` (or await `Finish::wait`) so subscribers still catching up drain the tail.

```rust
import.run().await?;
import.finish()?.wait().await;
```

### A timestamp rewind ends the import (#6426)

`import::Import::init` and `run` fail with the new `Error::Rewind` when a rendition's timestamps go backwards, such as a reset at an `EXT-X-DISCONTINUITY` under the same `EXT-X-MAP`.
`run` used to retry it, then evict the rendition and rejoin it as fresh tracks.
Treat the error as final and import the restarted source as a new broadcast.

### A failed timeline fails its listings

`export::Broadcaster::manifest` and `export::Rendition::media_playlist` return `Result<Option<String>>`.
Answer `Err(Error::TimelineFailed { .. })` with an uncacheable error (the server router uses 500 with `Cache-Control: no-store`) instead of serving a playlist.
`Rendition::playlist` fails the same way.
A failed timeline no longer ends a playlist with `EXT-X-ENDLIST` or turns the DASH manifest `static`; only a clean finish does.
A rendition whose own timeline failed is left out of the manifest and master playlist, while its siblings keep serving.

```rust
match rendition.media_playlist(query) {
	Ok(Some(playlist)) => { /* serve it */ }
	Ok(None) => { /* not ready yet: 404 */ }
	Err(err) => { /* the export failed: 5xx, no-store */ }
}
```

### Imports always anchor their own timebase (#5967)

`import::Config::timebase` and `Error::ForeignTimebase` are removed.
Drop the field from `Config`, and any match arm on `ForeignTimebase`.
Each import mints its timebase from the catalog it writes, and its first fragment anchors it.
A placed timebase only made redundant importers share timestamps; each importer now publishes its own epoch instead.

### A rendition waits for its own timeline (#6009)

`export::Broadcaster::rendition` and the `renditions()` cursor's `Event::Added` see a rendition only once the catalog advertises its own timeline, not as soon as its init is known.
An unhinted publisher declares that timeline after the rendition's first record, so feed media until then instead of waiting for `Added` or `Rendition::init` first.

### Discontinuities are absolute sequences

`export::segments::Segment::discontinuity` is a `u64` sequence, absolute within one `Broadcaster`, instead of a `bool` or a per-cursor count.
Store the value with each segment and use the first retained one as `EXT-X-DISCONTINUITY-SEQUENCE`.
A new `Broadcaster` starts a new sequence namespace: start a new recording, or map its sequences behind a discontinuity.

### Playlists are bounded (#4978)

Every timeline, durable ones included, lists a capped window of 256 segments.
`master.m3u8` lists a video rendition only once a segment starts at a sync point, and answers 404 until one can start.

### `Broadcaster::new` is synchronous

`Broadcaster::new` no longer awaits or fails.
A rendition whose sibling broadcast ends now ends its playlist instead of rebinding to a replacement.
Drop the `.await?`.
