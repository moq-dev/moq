# [M] Watch and publish take one shape and refuse old spellings

## Goal

`@moq/watch`, `@moq/publish`, and `@moq/room` construct every component the
same way, mean one thing by each name, and refuse the attributes and props
the last release spelled differently instead of aliasing them silently.

## Plan

- `Sync` stops taking `video`/`audio` jitter inputs; the decoders already
  hold `sync` and register their jitter. Today `element.ts`, room's
  `Member`, and moq.pro each bridge a `Signal` between `new Sync({ video })`
  and `new Video.Decoder(source, sync)` in four lines.
- Delete the silent aliases: the `latency`, `latency-min`, and `jitter`
  attributes and the `latency`/`latencyMin` props in `js/watch/src/element.ts`
  write `delay`/`buffer` today while only `latencyMax` throws, and the
  `jitter` attribute is a knob where the `jitter` getter is a readout.
  Setters throw like `latencyMax`; `get jitter()` stays a readout.
- Delete `Audio.Encoder.muted` in `js/publish`; `volume = 0` covers it and
  `<moq-publish muted>` keeps its capture-gating meaning like
  `<moq-watch muted>`.
- Watch constructors take a props object like publish:
  `new Video.Decoder({ source, sync, enabled })`,
  `new Video.Renderer({ decoder, canvas, visible })`, same for audio and
  text; positional arguments stay for identity only.
- `room.Local({ connection })` like `Room({ connection })`, wiring
  `connection.bandwidth` into its six encoders; today `Local` takes only
  `origin`, so no room publisher gets a reservation and the send-estimate
  split is silently off. `LocalProps` uses `GetterInit` like `RoomProps`.
- `Publish.Broadcast.in.maxAge` is `Getter<Time.Milli | undefined>`, not a
  bare `number`.
- `room.Local` exports the sources, captures, and broadcasts the demo
  touches, not seventeen readonly fields; `parseCatalogFormat` moves from
  the watch index to the element.
- Every publish source exposes `out.source: { video?, audio? } | undefined`;
  camera, screen, and file each have a different shape today and the
  element unwraps three ways.
- `<moq-watch>`'s `text` field is a source where `video`/`audio` are
  decoders; name the pair `text`/`textRenderer` (or `captions`/
  `captionsRenderer`). `<moq-publish>` wraps `sources` in `readonlys()` and
  drops `el.capture`, a duplicate of `el.video.capture`.
- `reload` becomes `announced` on `Broadcast`, the element attribute,
  room's `Member`, and the docs; it gates on the announcement and moq.pro
  reads the old name as retry. The released spelling refuses. `stalled`
  stays on both the catalog (publisher lagging) and the watch decoders
  (viewer waiting); the maintainer likes one word for both sides.
- Bugs on the same files: a stalled rendition silently overrides a manual
  quality pick (`video/source.ts`); `delay = "instant"` underruns audio
  unless the owner remembers to disable it, so `Audio.Decoder` gates it.
- Docs: `js/watch/README.md` and `doc/lib/js/watch.md` still list
  `latency*` and `jitter` attributes; `doc/lib/js/publish.md` uses
  `latencyMax` where `Track.Info` has `maxAge`.

Public API: breaking on @moq/watch, @moq/publish, and @moq/room, so on
dev. Wire: none. Consumers: `demo/web`, `doc/lib/js`, moq.pro's app.

## Related

- [Headless player](/quest/m2/watch-player.md) - the pipeline assembly every owner repeats, extracted after this settles the pieces
- [Audio time stretch](/quest/m2/watch-audio-time-stretch.md) - the audio ring behaviour this leaves alone
