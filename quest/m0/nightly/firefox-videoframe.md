# [S] Firefox's player closes every VideoFrame

## Goal

Nightly's Media job no longer fails Firefox's player on the console error "A
VideoFrame was garbage collected without being closed".

## Plan

Unowned in Nightly [40506938673](https://github.com/moq-dev/moq/actions/runs/40506938673) (2026-10-27, at 22808bbb2f).
[Firefox publishing](/quest/m2/publish-firefox.md) covers only the publisher
side. Find the player path that drops a decoded frame without `close()`, such
as one replaced, skipped, or discarded on teardown. The unclosed `VideoFrame`
is a leak in the player, not a console-noise filter to add.

Public API: none. Wire: none.
