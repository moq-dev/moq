# [XS] First shared FFI release

## Goal

Verify the first `moq-ffi-v*` tag that runs `Release FFI`. Condition: the
backport [#6730](https://github.com/moq-dev/moq/pull/6730) has merged to
`release`, and a `moq-ffi-v*` tag cut after it has run `Release FFI`. Check the
tag with `git ls-tree <tag> .github/workflows/` and its runs with
`gh run list --workflow release-shared-ffi.yml --event push`. Then verify the
items below, fix or quest what fails, and delete this quest.

## Plan

`moq-ffi-v0.4.16` through `moq-ffi-v0.4.18` were cut from a `release` without
[#5549](https://github.com/moq-dev/moq/pull/5549), so they ran the per-language
workflows. #6730 backports #5549, the attempt-scoped publisher check from
[#5573](https://github.com/moq-dev/moq/pull/5573), and the wrapper gates from
[#6084](https://github.com/moq-dev/moq/pull/6084), without the Shipping sizes
and C++ parts `release` lacks.

The Nightly side is verified: run 38897397428 covered the dry-run jobs (see
`.github/ffi.md`), and the Shipping sizes jobs of Nightlies 39779729461 and
40090959108 keep the `release/go-lib-<target>/` keys, with the second compared
against the prior history (a changed artifact set fails the report).

From the first shared tag:

- Each wrapper (`release-go.yml`, `release-kt-lib.yml`, `release-swift-lib.yml`)
  sees its publisher through the jobs API under the `workflow_run` token and
  releases or skips as its own publisher did.
- The Go, Kotlin and Swift packages publish from the shared artifacts.
