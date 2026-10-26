# [L] Undo and the scene model

## Goal

Every edit in `moq-studio` can be undone and redone from a history panel, and
the history survives autosave and restarts. The scene model distinguishes a
linked source (one capture shown in several scenes) from a clone with its own
filters, items and filters copy and paste between scenes and collections,
each canvas has a top layer that stays on across scene switches (a
downstream keyer), and filters can be set per scene item rather than only per
source.

## Plan

These are the most-voted OBS gaps and the most-installed plugins (Source
Clone, Source Copy, Downstream Keyer), and they all shape moq-scene's data
model, which the [layout track](/quest/m3/studio/compose/layout.md) also
serializes. So land them right after viewer composition, before more features
build on the model. Route every edit through one command type so undo,
autosave, and the layout track see the same changes. Autosave persists the
history beside the scene file, bounded by a count, and launch restores it;
an entry the loader cannot replay drops it and everything older. Update
`doc/bin/studio.md`.

## Related

- [Autosave](/quest/m3/studio/autosave.md) - the history must survive it
- [Scene editing](/quest/m3/studio/scene-editing.md) - rename, reorder, and resize go through the same commands
