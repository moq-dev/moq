# [M] Catalog track identity

## Goal

A track's catalog definition never changes for its name, so live and recorded
playback never guess which configuration applies to a group. A publisher whose
track configuration changes either refuses the change or publishes it under a
new track name or a new broadcast epoch. This is independent of DVR and does
not gate archives.

## Plan

Decided in the 2026-09-30 audit: track identity is immutable. Mutable
definitions with a configuration identity that groups reference (the old
option 2) are out, because they contradict the rule that a track name always
means the same content, and MoQ has no ETag-style invalidation.

Audit publishers in Rust and JS for the properties that change mid-track
today (codec/config bytes, resolution, audio layout, rendition metadata) and
make each one either refuse the change or mint a new identity: a new track
name through a [catalog alias](/quest/m1/catalog-track-alias.md) when the
catalog can keep both, or a new [broadcast epoch](/quest/m0/broadcast-epoch/README.md)
when the whole broadcast restarts. The catalog may still add and remove
tracks; a removed name is never reused for different content.

Cover codec changes, rendition switches, reconnects, and late joiners in Rust,
JS, and HLS/watch tests.

## Related

- [Catalog track alias](/quest/m1/catalog-track-alias.md) - lets a catalog list a new track name for a changed rendition
- [Broadcast epochs](/quest/m0/broadcast-epoch/README.md) - a restart is a new epoch rather than a changed track
- [Archive](/quest/m1/archive/README.md) - storage and replay consume the identity contract
