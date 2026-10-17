# [XS] A re-created encrypted track is refused, not dropped

## Goal

Publishing a track under a name the e2ee producer already carried fails
loudly instead of being silently left out.

## Plan

`moq-mux`'s e2ee producer carries each name at most once, and `moq-e2ee`
never releases a claim because reopening would restart its nonce counters,
so a re-created track today is just not carried. Decided 2026-10-17
(maintainer): refuse it. A track name always means the same content, so a
restarted encoder publishes a new name or a new broadcast.

Public API: a new error from the e2ee producer. Wire: none.

## Required

- [Rust protected publisher seams](/quest/m2/e2ee/rust-publish.md) - the producer that claims names
- [Dynamic names](/quest/m2/e2ee/dynamic-names.md) - the approved naming design can change the physical-name mapping this fixes
