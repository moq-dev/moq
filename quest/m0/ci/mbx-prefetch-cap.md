# [S] Bump mbx to the release that caps prefetch

## Goal

The Rust cache runs the first mbx release containing
[jdx/mr-boxington#951](https://github.com/jdx/mr-boxington/pull/951), so
prefetch no longer spends the download budget a job's own restores need. On
the first scheduled Nightly after the bump, no hosted Rust-cache job logs
"cached action is missing blob", each job keeps or raises its hits, and
`mbx-moq-dev` takes fewer PutObject 412s than on 2026-11-14 (86,387).

## Plan

Bump `version:` in `.github/actions/rust-cache/action.yml` (and the action's
pin if the release needs one), keeping `MBX_GC_MAX_SIZE=20GiB`, which upstream
recommends until then. On 2026-11-14, 20 Linux jobs still spent the full
20 GiB, almost all on prefetch, so a half-budget cap leaves 10 GiB for their
restores; check that budget refusals, now named as such, are gone.

Measure the night as [mbx-failed-puts](/quest/m0/ci/mbx-failed-puts.md)
does and post the PutObject and 412 counts on
[jdx/mr-boxington#950](https://github.com/jdx/mr-boxington/discussions/950),
as upstream asked, after re-reading its CONTRIBUTING.md and AI policy.

Public API: none. Wire: none.

## Waiting

- jdx/mr-boxington: a release containing #951 (`gh release list -R jdx/mr-boxington`).

## Related

- [Failed puts](/quest/m0/ci/mbx-failed-puts.md) - judges the 412s left after this
