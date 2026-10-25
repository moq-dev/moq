# Migrating @moq/publish

How to update your code for each breaking change, newest first.

## Unreleased

### `announce="never"` is gone

A broadcast that is never announced has no route at the relay, so nobody could subscribe to it.
Use `announce="always"` or `"source"`, and give a broadcast that should stay unlisted a hidden name with a `.`-prefixed segment, such as `room/.me`: discovery leaves it out, but it is still subscribable by name.

### Time units come from `Clock`

`Net.Time.Milli` and the other duration units moved to `@moq/clock`, which `@moq/publish` re-exports as `Clock`.
Replace `Net.Time.Milli` with `Clock.Milli`, or import from `@moq/clock` directly.

### `Broadcast.net` is read-only

`Broadcast.net` is a `Getter` instead of a writable `Signal`.
Read it with `get` or `peek` as before; the broadcast alone replaces the producer.

### AAC encoding support is a `Level`

`Support.Audio.aac` is `"full" | "none"` like `opus`, instead of a boolean.
Replace `aac === true` with `aac === "full"`.

### Video tracks advertise refresh (#6048)

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
