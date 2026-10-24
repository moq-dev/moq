# Migrating moq-hls

How to update your code for each breaking change, newest first.

## Unreleased

### Rendition URLs name the publisher instance

`Server` follows each instance that takes over a broadcast, and every URL the
master playlist and DASH manifest list now sits under that instance:
`/{broadcast}/@{instance}/{kind}/{rendition}/...` instead of
`/{broadcast}/{kind}/{rendition}/...`. `Route::parse` refuses the old layout,
and `Resource::{Media, Init, Segment, SegmentAt}` carry the `instance` they
name. `Broadcaster::master_playlist` and `Broadcaster::manifest` render the new
layout. An embedder that answers through `Server::respond` needs no change; one
that routes rendition URLs itself must pass the `@{instance}` segment through.

### Denied broadcast references report authorization failure

A rendition whose referenced broadcast is outside the source origin's scope now
returns `Error::Mux(moq_mux::Error::Moq(moq_net::Error::Unauthorized))` from
playlist (ranged too), init and segment reads, and from the segment cursor,
instead of behaving like missing media.
`Broadcaster::ready` and `Rendition::playable` settle on that refusal. Embedders
should answer it with an uncacheable 403; the HTTP server does so automatically.
Allowed renditions keep serving, and missing or disconnected media remains 404.

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
`Rendition::playlist` fails the same way, including for a range it used to refuse with `Error::RangeUnavailable`.
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

### Playlists take an optional range (#5705)

`export::Rendition::playlist(query)` is `playlist(range, query)`.
Pass `None` for the live or history playlist.

### Discontinuities are absolute sequences

`export::segments::Segment::discontinuity` is a `u64` sequence, absolute within one `Broadcaster`, instead of a `bool` or a per-cursor count.
Store the value with each segment and use the first retained one as `EXT-X-DISCONTINUITY-SEQUENCE`.
A new `Broadcaster` starts a new sequence namespace: start a new recording, or map its sequences behind a discontinuity.

### Playlists are bounded (#4978)

Every timeline, durable ones included, lists a capped window of 256 segments.
Set `Config::history` to list a durable timeline past the window.
`master.m3u8` lists a video rendition only once a segment starts at a sync point, and answers 404 until one can start.

```rust
let mut config = moq_hls::export::Config::default();
config.history = true;
```

### `Broadcaster::new` is synchronous

`Broadcaster::new` no longer awaits or fails.
A rendition whose sibling broadcast ends now ends its playlist instead of rebinding to a replacement.
Drop the `.await?`.
