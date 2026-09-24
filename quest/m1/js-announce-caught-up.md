# [S] @moq/net announce consumers know when they have caught up

## Goal

`@moq/net`'s announce consumer yields the same `Live` marker as the Rust
`announce::Consumer`, with the same ordering and per-source semantics, so a browser app can render "no
broadcasts" instead of a spinner that never resolves.

## Plan

Mirror the Rust shape (`announce::Event` is `Update(Update)` or `Live`), its
per-source guards (a session holds one per announce stream until the count,
ANNOUNCE_INIT, or a quiet stream lands it), and the marker-less fallback. Test the same cases
in JS. Public API: the announce consumer's yield type changes, so it lands
with the Rust break. Wire: none.
