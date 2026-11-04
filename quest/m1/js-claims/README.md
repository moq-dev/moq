# JS claims through a relay on lite-07

## Goal

A JS publisher that claims a path and answers its requests on demand
(`origin.requests()` and `request.serve()`) works through a relay on
`moq-lite-07-wip` the way it does in memory: a reader's `origin.request()`
resolves, re-serving a path from a fresh broadcast keeps the publisher's
session up, and a subscribe right after a track is replaced reaches the
replacement.

## Plan

Planned 2026-11-03 (maintainer) from Kixel's T3 tracks
([kixelated/t3code#1](https://github.com/kixelated/t3code/pull/1)), which
serves T3's subscriptions as claimed broadcasts per entity and hit each of
these against a local `moq-relay` built from `main`. Each child starts with a
failing test through a real relay on lite-07; the in-memory transport did
not show them. They rank in m1 because claim continuity on lite-07 is part
of the cut.

## Required

- [A claimed path resolves through `origin.request()`](/quest/m1/js-claims/request.md) - a reader's request resolves for a claimed path, as `follow()` already does
- [Re-serving a claimed path keeps the session](/quest/m1/js-claims/reserve.md) - a fresh broadcast whose first group is above 0 no longer closes the publisher with `invalid control encoding`
- [A subscribe after replacement reaches the new track](/quest/m1/js-claims/replace.md) - no `Unroutable` in the window after a track is replaced

## Related

- [A job's exact route continues its claim's readers](/quest/m1/announce-takeover.md) - the claim and route model these exercise
- [Front parking](/quest/m2/origin-front-parks.md) - `Unroutable` while covering routes move
