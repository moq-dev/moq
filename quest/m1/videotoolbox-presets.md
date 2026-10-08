# [XS] VideoToolbox honors encoder presets

## Goal

moq-video's VideoToolbox encoder (`rs/moq-video/src/encode/backend/videotoolbox.rs`)
maps each `Preset` to the controls VideoToolbox has, and `applied()` reports
what took effect. Today it applies real-time with no frame reordering for
every preset and reports `LowLatency`, so Balanced and Quality are silently
ignored.

## Plan

Decided 2026-10-08: split out of [macOS GPU input](/quest/m2/obs-macos.md),
since the mapping needs only an Apple machine, not the OBS GPU path.

- Balanced and Quality keep frame reordering off and turn real-time off,
  with quality or speed priority where the session accepts it. Follow how
  the NVENC and openh264 backends map presets and report `Applied`.
- A property the session refuses falls back to the next preset down, and
  `applied()` reports that, not what was asked.
- Measure per-frame encode time and bitrate at each preset on Apple
  hardware, and put the numbers in the PR.

Public API: none; `applied()` changes value for Balanced and Quality.

## Related

- [OBS native codecs](/quest/m1/obs-moq-video/README.md) - lists the per-backend preset gaps
- [macOS GPU input](/quest/m2/obs-macos.md) - feeds the same encoder from OBS
