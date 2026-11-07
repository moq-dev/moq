# [S] Review and agent rule

## Goal

Reviews and authoring agents catch the four implicit-state shapes the
[epic](/quest/m1/explicit-state/README.md) lists before they land. REVIEW.md
has a check, and AGENTS.md has one line, saying:

- a lifecycle is one enum or union whose variants carry their data;
- a terminal transition happens after the work that can fail, or records
  the failure;
- one value is never guarded by two locks or split across atomics.

## Plan

Decided 2026-11-07 (maintainer): ✅ both REVIEW.md and AGENTS.md. Not
chosen: REVIEW.md only, or no rule change. This is the prompt that the
repository's rule on changing agent instructions requires.

Follow `quest context`'s Prompting section: keep the AGENTS.md line short,
since it loads into every session, and put the detail and examples in
REVIEW.md. Point at the patterns already in the tree as the shape to copy:

- `PresentState` in `rs/moq-net/src/lite/session.rs`
- `Join.State` in `js/net/src/ietf/join.ts`
- `CompletionState` in `rs/moq-gst/src/sink/session.rs`
- the consuming `finish(self)` in `rs/moq-rtmp/src/server.rs`

Check the official guidance the Prompting section links before editing.

This child lands first so the other children's reviews apply the check.
