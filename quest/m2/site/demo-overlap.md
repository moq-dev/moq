# [S] Resolve demo and site overlap

## Goal

Each demo has one home. Decide which of `demo/web` (the local watch and
publish dev demo), `demo/boy` (rom.moq.dev), and the site's `/watch`,
`/publish`, and `/boy` pages survive, and delete the duplicates.

## Plan

Decided 2026-10-20 in planning: a separate quest after the move, so the move
stays a faithful snapshot and the overlap is judged with both copies in one
tree. Consolidating during the move and leaving the overlap were rejected.

Guidance:

- Weigh what each copy is for, such as local development against a dev relay
  versus a public page, before deleting one. A surviving page may need the
  other's features.
- Keep old URLs working. `doc/worker.js` already redirects the old `/demo`
  paths; redirect any public page that goes away instead of letting it 404.
- Update the docs, just recipes, and impact map entries that name a deleted
  demo.

## Required

- [Move moq.dev into site/](/quest/m2/site/move.md) - both copies must be in this tree
