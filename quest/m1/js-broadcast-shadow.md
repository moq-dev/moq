# [S] A JS broadcast shadows the claims beneath it, as in Rust

## Goal

A JS origin resolves a request beneath a broadcast published on it as Rust
does: the broadcast serves only its own path, yet as a claim on its subtree it
ranks in its claim tier, shadowing broader claims and superseding an older
epoch of its instance, so a request it alone covers most specifically fails
Unroutable instead of falling through to a broader claim. What a JS session
announces for a path then matches what a request for that path resolves to.

## Plan

Found by the local review of
[moq-dev/moq#9371](https://github.com/moq-dev/moq/pull/9371), and kept by the
maintainer when picking that PR to merge (2026-11-24, ✅ keep, rejected:
drop). `OriginState`'s
`bestEntry` (`js/net/src/origin.ts`) hands `mostSpecific` a local broadcast
only at the exact path requested, so one published above the path neither
shadows nor supersedes there. Rust's `RouteTable::best_route` ranks it in the
tier and drops it only when choosing what serves (`RouteEntry::serves`), as
`a_broadcast_here_shadows_a_broader_claim_shown_at_its_prefix` pins. JS
`presented` and `projectRoutes` already rank it that way, so today a session
rooted beneath `origin.route("src")`, with `origin.claim("")` beside it,
announces the broadcast for its root while a request for the root resolves
the broader claim, and a peer pinning the announced instance is refused.

Recommended: in `bestEntry`, add each announced local broadcast filed above
the path as a tier member through `bestOfTier`'s `serves` option, and return
nothing when the winner does not serve. Port the Rust test to JS, including
the epoch case (a broadcast at `src` at epoch 2 and a claim of the same
instance filed at `src/a` at epoch 1: a request for `src/a/b` is Unroutable).

Public API and wire: none. Behavior: a JS request beneath a broadcast no
longer falls through to a broader claim.
