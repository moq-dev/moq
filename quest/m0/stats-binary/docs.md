# [S] Stats format page

## Goal

A `doc/concept` page describes every stats broadcast and track: the path
layout, tiers, the three track kinds, both encodings (plain `.json` and the
`.json.z` merge-patch deltas in a group-scoped DEFLATE window), and the
counter semantics (cumulative, a decrease starts a fresh segment), in enough
detail for a consumer in another language to read them. The relay config page
and the moq-stats crate docs link to it rather than repeating it.

## Plan

Start from the moq-stats crate docs and the `[stats]` section of
`doc/bin/relay/config.md`, and move the wire description there. Add the page
to the VitePress sidebar.
