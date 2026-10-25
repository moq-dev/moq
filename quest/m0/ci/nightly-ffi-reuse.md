# [XS] First shared FFI release

## Goal

Backport `.github/workflows/release-shared-ffi.yml` to `release` now as
release-line maintenance (decided 2026-10-22, maintainer), so the next
`moq-ffi-v*` tag carries it. Condition: that tag has run `Release FFI`, and a
Nightly's `Shipping sizes` job has run since
[#5549](https://github.com/moq-dev/moq/pull/5549). Check the tag with
`git ls-tree <tag> .github/workflows/` and its runs with
`gh run list --workflow release-shared-ffi.yml --event push`; check sizes with
`gh run list --workflow nightly.yml --event schedule` and the `Shipping sizes`
job. Then verify the items below, fix or quest what fails, and delete this
quest.

## Plan

`moq-ffi-v0.4.16` (2026-10-16) was cut from a `release` without #5549, so it
ran the per-language workflows and verified nothing here; `moq-ffi-v0.4.17`
was likewise cut without it. `Shipping sizes` has run (Nightly
[39779729461](https://github.com/moq-dev/moq/actions/runs/39779729461),
2026-10-22): check its Go keys below, so only the tag condition remains.

The first scheduled Nightly after the change (run 38897397428, 2026-10-16)
already verified the dry-run side: the nested job names match the publisher
check, each packager selected its own targets from the shared artifacts, and
the Swift wrapper dry run passed. Its measured saving is in `.github/ffi.md`.

From the first shared tag:

- Each wrapper (`release-go.yml`, `release-kt-lib.yml`, `release-swift-lib.yml`)
  sees its publisher through the jobs API under the `workflow_run` token and
  releases or skips as its own publisher did.
- The Go, Kotlin and Swift packages publish from the shared artifacts.

From the first `Shipping sizes` run:

- The Go static libraries keep their `release/go-lib-<target>/` history keys,
  so the comparison shows no removed or added Go entries.
