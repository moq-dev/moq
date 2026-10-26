# [M] Interop covers late join and FETCH

## Goal

On the IETF matrix (`just test interop --versions ietf`), a
subscriber that joins mid-group receives the current group from object 0
(a joining FETCH before draft 22, a fill from draft 22) and a standalone
FETCH of a past group returns exactly its objects, for Rust and JS
publishers and subscribers.

## Plan

Decided with the maintainer on 2026-10-21: an in-tree case, not proposed to
the runner's spec.

This is the path the draft-22 public relay failures of 2026-10-21 hit: a
catalog written once is lost when a relay narrows a join to Next Object, and
the JS publisher refuses every FETCH. Publish a track whose first group
outlives the subscriber's arrival (a catalog written once, and a slow video
GOP) so the case cannot pass on timing.

Public API: none. Wire: none.

## Required

- [JavaScript FETCH](/quest/m1/js-fetch.md) - the JS publisher answers FETCH
