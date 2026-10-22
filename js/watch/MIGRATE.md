# Migrating @moq/watch

How to update your code for each breaking change, newest first.

## Unreleased

### AAC decoding support is a `Level`

`Support.Audio.aac` is `"full" | "partial" | "none"` like `opus`, instead of a boolean, since the libav fallback now decodes AAC where `AudioDecoder` is missing.
Replace `aac === true` with `aac !== "none"`, or with `aac === "full"` to require a native decoder.

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
