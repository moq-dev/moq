# Migrating @moq/publish

How to update your code for each breaking change, newest first.

## Unreleased

### `muted` and `invisible` keep capture open

`<moq-publish muted>` and `invisible` now stop only the encoder, so the microphone and camera stay open and the browser's capture indicators stay lit.
Unmuting no longer waits on getUserMedia, a new AudioContext, or a camera reopen.
A camera source opens both devices even when it starts muted or hidden.
To release the devices, clear `source` or remove the element; to release one and keep the other, build the pipeline with the library and toggle `Source.Microphone` or `Source.Camera` `enabled`.

### Element reflection and invalid configuration

Property and built-in UI changes now reflect into attributes. Invalid source/preview/backend/announce modes, URLs, and version lists throw instead of selecting defaults; remove an attribute to use its default.
Catch those property setter errors. A browser reports malformed markup through its normal `error` event, and the element suspends media until those invalid attributes are corrected or removed. A valid property assignment also corrects that attribute. An empty, duplicate, or unsupported `versions` list now throws from the setter instead of failing on `el.connection.error`; check a list up front with `Net.Connection.checkVersions`.
A `File` source reflects as `source="file"` while retaining the selected `File`; reflection does not reopen the picker or replace it with a source selector.

### `announce="never"` is gone

A broadcast that is never announced has no route at the relay, so nobody could subscribe to it.
Use `announce="always"` or `"source"`, and give a broadcast that should stay unlisted a hidden name with a `.`-prefixed segment, such as `room/.me`: discovery leaves it out, but it is still subscribable by name.
An element still set to `never`, or any other invalid value, throws and does not announce; `announce` then reads `undefined`.

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
