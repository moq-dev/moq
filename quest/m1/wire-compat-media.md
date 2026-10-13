# [S] Wire compatibility always exercises the compat reader's media path

## Goal

`just test wire-compat` runs at least one current-to-current and one
released-to-released media cell through the compat reader
(`test/interop/compat/client.ts`, selected by `INTEROP_COMPAT_CLIENT`), so a
regression in that reader's media path fails the nightly even while planned
breaks skip every cross-version media cell.

## Plan

Found on [#5444](https://github.com/moq-dev/moq/pull/5444): the
`catalog-archive` planned break skips all 24 media cells until a release
carries #4034, so the new reader's media path ran only by hand. A planned
break is about a version pairing, so same-version cells are never covered by
one and stay meaningful.

Start after #5444 merges: it adds the reader's `subscribe` command and the
`INTEROP_COMPAT_CLIENT` selector this quest relies on.

Decided 2026-10-13 (maintainer): m1. Add the two control cells to the
matrix in `test/interop/compat.sh` and `resolve.ts`, reusing the existing
lanes rather than a new harness. Pick the lite version the released packages
and binaries both negotiate. Prove each cell fails when the reader's media
path is broken (for example, a reader that never decodes a frame), then
restore it. Runs in the nightly Interop's wire-compat step; never dispatch the
nightly on a branch.

Rejected 2026-10-13: a separate tsc pass of the compat reader. `just js check`
already typechecks `test/interop/compat/*.ts` against the checkout's packages
through `test/tsconfig.json`, and running a released lane catches API the
released packages lack.

Public API: none. Wire: none.

## Related

- [#5444](https://github.com/moq-dev/moq/pull/5444) - the compat reader and the re-measured planned breaks
