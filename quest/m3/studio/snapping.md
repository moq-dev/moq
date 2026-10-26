# [S] Snapping and guides

## Goal

Moving and resizing items in `moq-studio`'s preview snaps to the canvas
edges and center, to other items' edges and centers, and to an optional grid,
with smart guides and equal-spacing hints drawn while dragging. A modifier key
turns snapping off.

## Plan

OBS only now adds axis snapping. Snapping is a preview interaction, so it
lives in the studio, not in moq-scene or the layout track. Moves still go
through the scene model's commands so they undo.

## Related

- [Undo and the scene model](/quest/m3/studio/scene-model.md) - snapped moves are ordinary commands
