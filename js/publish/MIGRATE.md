# Migrating @moq/publish

How to update your code for each breaking change, newest first.

## Unreleased

### `Encoder.cut()` is not rate limited, and video tracks advertise refresh (#6048)

`Encoder.cut()` no longer spaces keyframes 500 ms apart: every call forces one on the next frame, so frequent calls produce all-keyframe video.
Debounce your own calls if you relied on the spacing.
Every `Broadcast.video()` track now advertises a refresh ratio of 2, so a producer registered there must open a group with a keyframe whenever `track.demand().wanted` passes the largest group.
The bundled `Encoder` already does; a custom producer has to as well.

### A disabled rendition stays in the catalog (#4915)

`enabled: false` keeps the rendition in the catalog with `enabled: false` instead of removing it, and viewers skip it.
Code that read a missing rendition as disabled should check `catalog.video?.renditions[name]?.enabled === false`.

### Tracks persist for the broadcast's life (#5068)

`Rendition.close()` no longer closes the network track.
A `maxAge` change, or replacing a closed rendition track, recreates `broadcast.net`, and registering a name under a different kind on the same network broadcast throws.

### `OpusConfig.usedtx` is removed

Opus DTX is always off, since Chromium's DTX output shifts the audio timeline.
Remove the field; a plain-JS caller still passing it is ignored.

## 0.4 -> 0.5

### Publish takes an origin

Pass `origin: connection.origin` to `Publish.Broadcast` instead of a `connection` signal.
