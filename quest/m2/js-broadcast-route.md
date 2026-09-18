# [S] JS Announce.Broadcast follows any matching claim

## Goal

`Announce.Broadcast` in `js/net/src/announced.ts` goes live on any announced
pattern that matches its path, the way Rust `Consumer::routed` does. Today it
accepts only the subtree claim `path/**` or a literal claim of the exact
path, and treats every other wildcard claim as a capability rather than an
inventory, so a browser watching `game/alice` stays offline while a service
advertising `game/*` serves it and a Rust client at the same path plays.

## Plan

Replace the `covers` check with the Rust rule: the event's pattern is a
prefix of the path or `matches` it. More than one claim can now match
(`game/*` and `game/**` both cover `game/alice`), so both paths in
`Broadcast` keep the set of active matching patterns instead of one flag,
and go offline only when a retraction empties it. Keep the
redundant-re-announce handling and the blind-consume fallback. Add the
wildcard case and the two-claims-one-retraction case to the announced tests
beside the subtree and literal ones, and note the rule in `doc/lib/js/net.md`
where `Announce.Broadcast` is described. Public API: none. Wire: none.

## Required

- [Merge dev](/quest/m1/merge-dev.md) - the pattern-valued announce events only dev has
