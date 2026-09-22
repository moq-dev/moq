# [S] @moq/net announce consumers know when they have caught up

## Goal

`@moq/net`'s announce consumer gains the caught-up signal the Rust consumer
gets from [Caught up](/quest/next/cli-inspect/caught-up.md), with the same
semantics and a mirrored name, so a browser app can render "no broadcasts"
instead of a spinner that never resolves.

## Plan

Mirror the Rust shape and its marker-less fallback. Test the same cases in JS.
Public API: additive on @moq/net. Wire: none.

## Required

- [Caught up](/quest/next/cli-inspect/caught-up.md) - settles the API shape this mirrors
