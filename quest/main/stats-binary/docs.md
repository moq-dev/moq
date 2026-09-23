# [S] Stats format page

## Goal

A `doc/concept` page describes every stats broadcast and track: the path
layout, tiers, the three track kinds, both encodings (`.json.z` merge-patch
and `.fb.z` FlatBuffers), the counter semantics (cumulative, a decrease
starts a fresh segment), and how a consumer in another language generates a
reader from the `.fbs`. The relay config page and the moq-stats crate docs
link to it rather than repeating it.

## Plan

Start from the moq-stats crate docs and the `[stats]` section of
`doc/bin/relay/config.md`, and move the wire description there. Add the page
to the VitePress sidebar.

## Required

- [FlatBuffers flavor](/quest/main/stats-binary/flatbuffers.md) - the encoding the page documents
