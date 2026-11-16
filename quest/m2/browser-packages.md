# [M] Room, publish, and OBS names say what they hold

## Goal

Three browser packages lose their confusing names before the cut publishes
them:

- `@moq/room`: `Room.Local` nests each device's state under the device:
  `local.camera`, `local.microphone`, and `local.screen` each hold their
  `enabled` switch, capture source, and what they publish, so no two fields
  named after one device are different objects.
- `@moq/publish`: `<moq-publish>`'s `capture="hold|release"` attribute has a
  name that says what is held, and the feedback path is `echo-path` as an
  attribute, `echoPath` as a control, and `echoPath` as the
  `Publish.Broadcast` input.
- `@moq/obs`: a destination's `layers` is `renditions`, and the settings
  type `configure` takes is `Config`.

## Plan

Decided 2026-11-16 (maintainer, /quest-plan of the pre-cut audit):

- ✅ `Room.Local` nests by device (rejected: moving only the audio addition
  beside its device; keep). AGENTS.md: "Nest related API ... under their
  shared role."
- ✅ Rename `<moq-publish capture>` and align `echo-path`, `echoPath`, and
  `echo` (rejected: only the `echoPath` rename; keep).
- ✅ `@moq/obs` says `renditions` and `Config` (rejected: keep). `layers`
  is OBS's word; the rest of the stack says renditions.

Facts found by the audit, re-checked at `80ca9787e3`:

- `js/room/src/local.ts:62-89`: `webcam` is the `Source.Camera` while
  `camera` is its `Publish.Broadcast`; `share` is the `Source.Screen` while
  `screen` is its broadcast; `microphone` is a `Source.Microphone` while
  `audio.microphone` holds its `volume` and `root`; `audio.screen` is a
  third object beside `screen`. `cameraEnabled`, `microphoneEnabled`,
  `screenEnabled` (`:47-51`), `cameraCapture`, and `screenCapture`
  (`:74-76`) sit flat beside them. The only caller outside the package is
  `demo/web/src/meet.ts` (5 references).
- `capture` (`js/publish/src/element.ts:55`, `CaptureMode` at `:79`) is
  "what a muted or invisible track does with its capture". `el.capture` is
  that policy string while `el.video.capture` and `el.audio.capture` are
  `Capture` objects, and HTML's own `capture` attribute on `<input>` picks
  a camera. The name was the implementing agent's pick in
  [moq-dev/moq#7746](https://github.com/moq-dev/moq/pull/7746), not a
  recorded decision. No page or doc sets it; only
  `test/interop/clients/js/publish-ui.browser.ts` does.
- The publish attribute is `echo-path` and the control `echoPath`
  (`element.ts:54`, `:175`), but the input under them is
  `Broadcast.in.echo` (`js/publish/src/broadcast.ts:86`). The CLI flag is
  `--echo-path`. `<moq-watch echo>` takes the viewer's feedback name, one
  path segment (`js/watch/src/element.ts:119`), which is a different
  thing and keeps its name.
- `Destination.layers` (`js/obs/src/obs.ts:62`) is documented as "video
  renditions to publish", and [layers](/quest/m3/layers/README.md) uses
  the word for composed broadcasts. `configure(publish: Publish)`
  (`:255`) names its settings type `Publish` (`:73`), the sibling
  package's name. `@moq/obs` is unpublished.

For the PR:

- Room, recommended shape: `local.camera.{enabled, source, capture,
  broadcast}`, `local.microphone.{enabled, source, volume, root}`,
  `local.screen.{enabled, source, capture, broadcast, audio}`. The props
  that seed the switches (`cameraEnabled` and its two siblings, `:22-26`)
  nest the same way. Leave `TRACK.location` and `HangCatalog.location`
  (`js/room/src/metadata.ts:36`, `:91`): the package doc calls them
  app-defined extensions, and removing them is not part of this decision.
- Capture attribute, recommended name: `devices="hold|release"` (the
  audit's pick). It also governs a screen share and a file, so if that
  reads wrong, `idle="hold|release"` is the alternative; pick one and say
  why in the PR. The type, the control, and the parse error follow the
  attribute. It is new in this cycle, so it needs no tombstone attribute.
- Rename `BroadcastInput.echo` to `echoPath`; the catalog's `echo` section
  keeps its name, since that is the wire.
- `MAX_LAYERS` and the range error in `obs.ts` follow `renditions`; the
  OBS setting `WHIPSimulcastTotalLayers` keeps OBS's name. Update
  [the dock's simulcast check](/quest/m3/obs-multitrack-check.md) if it
  names the field.
- Update each package's `api/*.api.md`, README, and `MIGRATE.md`
  (`js/obs` has none: it has never been published), plus
  `doc/lib/js/publish.md`.

Tests: the existing room, element, and obs suites pass under the new
names; `just dev` shows the meet demo publishing camera, microphone, and
screen.

Public API: breaking for `@moq/room` (`Room.Local` and its props),
`@moq/publish` (one element attribute and control, one `Broadcast` input),
and the unpublished `@moq/obs`. Wire: none.

## Related

- [@moq/ui is a workspace package](/quest/m2/ui-private.md) - the packaging change to the same overlays
- [Unused media API](/quest/m2/unused-api/media.md) - the browser exports removed beside these renames
