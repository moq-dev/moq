# [M] A VR demo reads the catalog's spatial fields, tested end to end

## Goal

`demo/vr` plays a broadcast in a WebXR headset (or a desktop tab) and picks
its layout from the hang catalog: video `projection` and its `fov` choose a
flat screen or an equirectangular sphere, `stereo` crops each eye from a
left-right or top-bottom frame, and an audio `layout` of `ambisonic` decodes
the sound field around the head. A harness imports a small VR180 left-right
fixture with first-order ambisonic Opus, publishes it through a relay, and
asserts the demo sees and applies the right projection, fov, stereo, and
layout.

This gives the fields that
[moq-dev/moq#5543](https://github.com/moq-dev/moq/pull/5543) added and
[moq-dev/moq#7308](https://github.com/moq-dev/moq/pull/7308) fills on import
their first reader, so they ship with the cut instead of being deleted.

## Plan

Decided 2026-11-14 (maintainer): keep the fields and finish
[moq-dev/moq#6373](https://github.com/moq-dev/moq/pull/6373) so they are
exercised (rejected: deleting them before the cut, as the feature review's
`catalog-vr-fields` quest in
[moq-dev/moq#8764](https://github.com/moq-dev/moq/pull/8764) recommended).
✅ m1, since a demo and a harness break nothing (rejected: m2, which holds
breaking work).

- Build on @nuts-rice's commits from `nuts-rice/moq:webxr_vr_demo`, keeping
  their authorship; the PR credits them and links #6373. Maintainer edits
  are off on their fork, so the work lands from a moq-dev branch.
- Drop #6373's catalog plumbing that #7308 made redundant:
  `moq_mux::catalog::Spatial`, its producer and track hunks, the `import
  --projection/--stereo/--audio-layout` flags, and the `pub` on the hang
  `check` helpers. The importers fill the fields from MP4 `st3d`/`sv3d`,
  Matroska, and Opus family 2. Adding CLI or catalog API needs the
  maintainer first.
- `@moq/watch` drops every Opus `description`, so Chromium refuses Opus past
  stereo ("description is required for multi-channel Opus"). Passing the
  catalog's OpusHead to both the support probe and the decoder, as the Rust
  importers intend, is part of this quest, since the ambisonic decode needs
  it. Chromium then trims the header's pre-skip itself, again after every
  flush (measured: 648 of 960 frames), so WebCodecs gets a copy with the
  pre-skip zeroed and the decoder's `Terminal` stays its one owner, gap
  resumes included.
- The harness publishes the fixture through `moq import fmp4`, so it proves
  the importer's side too, and runs nightly in `interop.yml` beside the
  portrait harness, outside the merge gate.

Public API: none (a behavior fix in `@moq/watch`). Wire: none.

## Related

- [fMP4 import fills the ambisonic layout](/quest/m5/mux-vr-ambisonic-fmp4.md) - the harness's fixture exercises that importer path
