# [M] One reusable workflow for the moq-ffi releases

## Goal

`release-dart-ffi.yml`, `release-go-ffi.yml`, `release-kt-ffi.yml`,
`release-swift-ffi.yml`, and `release-py-ffi.yml` share half to two thirds of
their lines: the moq-ffi cross-target build matrix and the artifact staging.
That shared half becomes one reusable `release-ffi.yml`; each language keeps
only its packaging and publish steps.

## Plan

- Measure the shared lines first (`comm` over the sorted files shows 74 to
  176 common lines per pair) and confirm the matrix targets agree. Where a
  language needs a different target set (iOS simulator for swift, Android
  ABIs for kt and dart), make the target list an input rather than forcing
  one matrix.
- `release-ffi.yml` on `workflow_call`: builds `rs/moq-ffi` for the requested
  targets, uploads one artifact per target, and outputs the version parsed
  from the tag. Callers download the artifacts and run their `just <lang>
  package` and publish recipes.
- `release-dart-ffi.yml` and `release-go-ffi.yml` also run on `pull_request`,
  where `GITHUB_REF` is not a `moq-ffi-v*` tag and `parse-version` fails; they
  read the version from `rs/moq-ffi/Cargo.toml` instead. The reusable
  workflow keeps that fallback (an input or a ref check), or those PR runs die
  before building.
- Callers keep their `name:` and triggers, as the binary quest did, so
  `alert.yml` and the `workflow_run` chains (`release-go.yml`,
  `release-py.yml`, `release-swift-lib.yml`) are untouched.
- Verify with `just gh check`; the next tagged `moq-ffi-v*` release is the
  end-to-end check.

## Required

- [Binary release workflow](/quest/m2/tooling/release-binary.md) - proves the reusable-plus-callers pattern before it is applied to five files
