# [M] JS packages' small surface fixes

## Goal

- `@moq/net`:
  - A resolved `Track.Info` uses `null` for "none" (`maxAge`, `timescale`);
    `Track.infoDefaults` and the `Track.rescale` alias go.
  - `Connection.Established` becomes `Session`, and the orphan
    `Connection.Extensions` type goes.
  - `readBool` goes; it has no caller and no Rust mirror.
  - `RouteOptions.instance` takes `{ id, epoch }`, like Rust's
    `with_instance(instance, epoch)`.
- `@moq/qmux` exports `Session` by name instead of an anonymous default.
- `@moq/watch`: the decoder and source plumbing outputs are `@internal`, the
  renderer's output is a named `Video.RendererOutput`, and `<moq-watch>`'s
  `player` property goes private, so each component has one path.
- `@moq/publish`: `Rendition.onDemand` becomes an enum, `Broadcast.join`
  becomes a `Getter`, and the five internal source helpers are unexported.
- `@moq/ui` and `@moq/video` mark their helpers `@internal`; only `Support`,
  `Report`, `Show`, `Level`, and `Status` stay public in `@moq/ui`, and
  `rotateVideoDimensions`, `Dimensions`, `Presentation`, and
  `Preview.Transcode` leave `@moq/video`'s public API.
- `@moq/boy` stops exporting `Game`, `GameConfig`, `KEY_MAP`, and its
  `Moq` and `Watch` namespace re-exports, keeping the element and status
  schemas.
- `@moq/auth`'s `Key.sign` takes one spelling for caps.

## Plan

Found by the 2026-11-07 API shape audit (JS core and browser media); see
the [epic](/quest/m2/api-shape/README.md). Durations are
[js-time-types](/quest/m2/js-time-types.md); hosted assets and the
`supported` default are m1 quests. Each change updates its package's
`api/*.api.md`, and moq.dev's npm consumer is checked for each removed
export.
