# [M] Layout track

## Goal

A studio broadcast carries a layout track: moq-scene's live scene state
(items, the tracks they show, transforms, transitions, and each element's
priority and whether a viewer may toggle it) as a hang JSON track on the
media clock. Any host that reads it composites the same frame the studio
shows. The hang catalog and `drafts/draft-lcurley-moq-hang.md` specify it.

## Plan

Serialize moq-scene's own model rather than a second layout schema, so the
studio, server, and browser cannot drift. Version it and refuse an unknown
version loudly. Keep it free of local paths and credentials: items name
tracks and contribution references, never files. A late joiner needs the
current state, so use the JSON track's snapshot mode.

## Required

- [Source tracks](/quest/m3/studio/compose/sources.md) - the tracks a layout names
