# MoQ in obs-studio

## Goal

A MoQ output and service ship inside obs-studio itself, native rather than a
plugin, and are configurable through obs-websocket like the other streaming
services. Until that merges, the plugin (`cpp/obs`) remains the vehicle and
keeps shipping.

## Plan

This README holds the port and merge work: carry the plugin's output and
service into obs-studio's tree in the shape its maintainers ask for, then
land it upstream. Shape it around their answer to
[the condition](/quest/m2/obs-studio/maintainers.md), not ahead of it.

Agents may prepare the proposal and the code, but never post to obs-studio
(issues, PRs, comments, forums) without the maintainer's explicit approval
for that post. The maintainer does the outreach.

Open, for when the port starts:

- How obs-studio takes the Rust-built library: a prebuilt dependency like
  its other media libraries is the likely shape, through the generated C++
  package rather than the hand-written libmoq.
- What stays in the plugin: the MoQ Source and the dock may remain a plugin
  even once the output is native.

## Required

- [obs-studio maintainers agree](/quest/m2/obs-studio/maintainers.md) - condition: agreement in principle to a native MoQ output and service

## Related

- [First C++ package release](/quest/m2/release/cpp-release.md) - the published C++ package the port would build on
- [OBS native codecs](/quest/m2/obs-moq-video/README.md) - the plugin's codec work, which stays plugin-side
