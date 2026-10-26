# [M] Scene editing

## Goal

In `moq-studio`, scenes can be renamed and reordered, and the canvas can be
resized while live, with no restart.

## Plan

Decided 2026-10-26 (maintainer): a resize while publishing adds a new video
rendition under a new track name in a catalog update, and viewers switch to
it. A track name never changes what it means (AGENTS.md), so the old track
ends rather than changing resolution mid-stream. The compositor reallocates
its canvas textures and the program encoder restarts on the new size.

A rename updates every reference to the scene in the collection. Reordering
is the order of the scene file's list. Update
`doc/bin/studio.md`.

## Related

- [Multiple canvases](/quest/m3/studio/multi-canvas.md) - canvases of different sizes at once, sharing the resize path
