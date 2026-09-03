# [M] Fetch and catalog

## Goal

An uncached FETCH encodes at the ladder's shared applied target instead of
opening a fresh encoder at the configured maximum, and a source catalog
refresh cannot erase current rung state.

## Plan

Two independent leaks of controller state, both cheap to close once the
controller owns it.

The FETCH path opens a new encoder at the configured maximum for every
requested group, so a stalled ladder still burns full-rate encodes on demand.
Make it read the controller's applied target and participate in the same
allocation. A stalled rung stays manually fetchable, and cache hits are
untouched.

Catalog mutations publish full HANG, HANGZ, and MSF snapshots today. Coalesce
every rung state change from one controller iteration into a single
publication, and make a later source catalog snapshot compose with current
generated-rung state rather than overwrite it.

Acceptance: a fresh FETCH using the shared applied target while a cache hit
encodes nothing, and one source catalog refresh leaving stalled rung state
intact.

## Required

- [Controller](/quest/m1/ladder/controller.md) - there is no shared applied
  target to read until the controller owns one
