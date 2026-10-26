# [S] Studio autosave

## Goal

`moq-studio` writes every change to the scene collection to its scene file
shortly after it happens, with no save step and no prompt on quit. A crash
loses at most the edits inside the debounce window. A failed write shows in the
UI rather than being dropped.

## Plan

Decided 2026-10-26 (maintainer): autosave, not a dirty flag and a quit prompt.
Debounce bursts such as dragging a slider, flush on quit, and write
atomically (temporary file plus rename) so a crash never leaves a half-written
file. The scene file stays diffable, so an autosave that changes nothing
writes nothing. A scene path that does not exist yet is created by the first
autosave, replacing the guide's "created on the first save" and
**File > Save**. Update `doc/bin/studio.md`.
