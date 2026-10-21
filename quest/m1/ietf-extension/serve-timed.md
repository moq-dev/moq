# [S] Timed moq-transport tracks cross a lite-07 mesh

## Goal

A timed track ingested over moq-transport keeps its timestamps across a
lite-07 hop to a peer that negotiated the `ietf` extension, so a relay mesh
carries them end to end. A peer without the extension still gets the track
untimed.

## Plan

Decided 2026-10-21 with the maintainer: m1, under moq-transport passthrough.

[moq-dev/moq#6150](https://github.com/moq-dev/moq/pull/6150) marks a timed
track received over moq-transport with the crate-private
`track::Info::unchecked` and serves it untimed to moq-lite, so its
timestamps stop at the first lite hop.
[moq-dev/moq#6107](https://github.com/moq-dev/moq/pull/6107) gives every
moq-transport track `track::Info::ietf()`, its ingress version. Derive
`unchecked` from `ietf()` instead of keeping a second flag, and let the lite
publisher serve such a track timed to an `ietf` extension peer, which knows
the timestamps came from moq-transport and are unchecked.

Mirror it in js/net, where `Info.unchecked` is `@internal`. If the timed
encoding on that hop needs a wire change, update
`drafts/draft-lcurley-moq-lite.md` (or the extension's draft) in the same PR.

## Required

- [Received timestamps are media time](/quest/m1/lite-time-provenance.md) - adds `unchecked` and the untimed serving this changes
- [Ingress-version provenance](/quest/m1/ietf-extension/lite.md) - adds `Info::ietf()`, which `unchecked` derives from
