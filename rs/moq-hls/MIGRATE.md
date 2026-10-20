# Migrating moq-hls

How to update your code for each breaking change, newest first.

## Unreleased

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
