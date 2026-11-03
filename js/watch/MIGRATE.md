# Migrating @moq/watch

How to update your code for each breaking change, newest first.

## Unreleased

### Element reflection and invalid configuration

Property and built-in UI changes now reflect into attributes. Invalid rendering/catalog modes, URLs, numbers, durations, and version lists throw instead of selecting defaults; remove an attribute to use its default.
Catch those property setter errors. A browser reports malformed markup through its normal `error` event, and the element suspends media until those invalid attributes are corrected or removed. A valid property assignment also corrects that attribute. An empty, duplicate, or unsupported `versions` list now throws from the setter instead of failing on `el.connection.error`; check a list up front with `Net.Connection.checkVersions`.
`broadcast`, `video`, `audio`, `renderer`, and `sync` are TypeScript `readonly` fields. Configure the existing components through their inputs and `controls` instead of replacing them; use a headless `Player` for a different pipeline.
`announced=false` reflects as `announced="false"`; `captions` and `catalog-format` now reflect too.

### Muting preserves the selected volume

`<moq-watch>` keeps `muted` and `volume` independent, in either attribute or property order.
Muting no longer writes zero to the `volume` property or attribute, and unmuting keeps the selected level, including zero.
Setting `volume = 0` no longer sets `muted = true`, and setting a positive volume no longer clears mute.
Set `el.muted` explicitly to change mute state; read it instead of `el.volume === 0` to identify mute.
For audible state, check both: `!el.muted && el.volume > 0`.
Audio downloads still stop at zero volume, independently of the mute control.
Programmatic level changes preserve mute; the UI slider still explicitly unmutes when moved above zero.

### Player inputs and `<moq-watch>` controls nest by media kind

Move each per-media input under `video`, `audio`, or `text`, on `new Player({...})`, on `player.in`, and on the element's `controls`:

| Before | After |
| --- | --- |
| `target` | `video.target` |
| `canvas` | `video.canvas` (Player only) |
| `visible`, `backend` | `video.visible`, `video.backend` |
| `volume`, `muted` | `audio.volume`, `audio.muted` |
| `captions` | `text.target` |
| `container` | `text.container` (Player only) |

For example, `new Player({ canvas, muted })` becomes `new Player({ video: { canvas }, audio: { muted } })`, and `el.controls.volume` becomes `el.controls.audio.volume`.
`paused`, `delay`, and `buffer` stay flat, as do the element's attributes and their properties (`el.muted`, `el.captions`, ...).
Pin an audio rendition the new way, with `controls.audio.target` or `new Player({ audio: { target: { name } } })`.

### `Sync` drops its duplicate and decoder-only surface

Read `Sync.out.delay` instead of `Sync.out.jitter`, and the element's `sync.out.delay` instead of its `jitter` getter; they always held the same value.
Stop passing `probe` to `new Sync(...)`; it was never read.
`Sync.now()` returns `Milli | undefined`.
`received`, `wait`, and `register` are internal to the built-in decoders, and `epoch` and `skipped` are gone: `wait` now resolves `true` to present the frame or `false` to drop it.
`<moq-watch>` no longer observes `reload`, `latency`, `latency-min`, or `jitter`, nor has the `latency`, `latencyMin`, `latencyMax`, or `reload` properties; use `announced`, `delay`, and `buffer` (see 0.5 -> 0.6).

### `Video.Source.out.error` is an `Error`

`Video.SourceError` is gone, and `Video.Source.out.error` holds an `Error` instead of the `"unsupported"` string, like every other `out.error`.
Replace `error === "unsupported"` with `error?.name === "NotSupportedError"`.

### Time units come from `Clock`

`Net.Time.Milli` and the other duration units moved to `@moq/clock`, which `@moq/watch` re-exports as `Clock`.
Replace `Net.Time.Milli` with `Clock.Milli`, or import from `@moq/clock` directly.

### `Broadcast.relativeBroadcast` is internal

`relativeBroadcast` is gone from the published types; the decoders resolve each rendition's broadcast themselves.
Read the media through `Video.Decoder` or `Audio.Decoder` instead.

### AAC decoding support is a `Level`

`Support.Audio.aac` is `"full" | "partial" | "none"` like `opus`, instead of a boolean, since the libav fallback now decodes AAC where `AudioDecoder` is missing.
Replace `aac === true` with `aac !== "none"`, or with `aac === "full"` to require a native decoder.

### CMAF decoding loads on demand

Allow your bundler to emit and serve the dynamic `@moq/hang/cmaf` chunk when using CMAF renditions.
Legacy and LOC playback do not download it; player calls and catalog formats are unchanged.
Replace access through `Watch.Hang.Container.Cmaf` with an import from `@moq/hang/cmaf`.

### A cleanly ended catalog keeps playing

When the catalog track ends cleanly, the player keeps its catalog and `"live"` status until the broadcast closes or is withdrawn, instead of going `"offline"` at once.
Wait for `Broadcast.out.status` to report `"offline"` to learn that a finished broadcast ended, not for the catalog to clear.

### A newer catalog reports `unsupported`

Handle `"unsupported"` in `Broadcast.out.status` by asking the viewer to update their player.
This status replaces `"offline"` for a fetched catalog needing a newer reader and `"loading"` for a manual one.
It is separate from `"error"`, which still reports an origin refusal through `Broadcast.out.error`.

### `"auto"` delay is measured (#4162)

`"auto"` is sized from how late frames arrive instead of derived from RTT.
A numeric `delay` is taken literally instead of having the rendition's own delay added on top.
`Sync.out.jitter` always equals `Sync.out.delay`, and `"auto"` with no decoder registered resolves to 0 instead of 100 ms.

### Staleness is `maxDelay`

Rename `Sync.out.maxAge` to `Sync.out.maxDelay`.

### Disabled renditions are never selected (#4915)

A rendition the catalog marks `enabled: false` is skipped, even when named.
With no video rendition selectable, the picture clears to black.

### `announced` off stays on one publisher

With `announced` off, the player no longer moves to a later publisher at the same path.
Turn `announced` on to follow restarts.

## 0.5 -> 0.6

### Delay attributes need a unit (#3396, #3817)

`latency`, `latency-min`, `latency-max`, and `jitter` are `delay` and `buffer`, each with a unit.
`reload` is `announced`.

```html
<moq-watch url="..." name="demo" delay="100ms" buffer="30s" announced></moq-watch>
```

### `Watch.Broadcast` is `Watch.Player`

Rename `Watch.Broadcast` to `Watch.Player` and pass `origin: connection.origin` instead of `connection`.
