# [S] Build moq-ffi once in the Dart check

## Goal

The Dart check builds the release `moq-ffi` library once, not twice, and
`dart test` still exercises a library built from this checkout.

## Plan

- Today `sh/dart/generate.sh` builds it, then `dart test`'s build hook builds it
  again, probably because the hook's filtered environment changes Cargo's
  fingerprint. Check [run 38853884676](https://github.com/moq-dev/moq/actions/runs/38853884676)
  spent about 6.7 and 6 minutes on the two builds.
- Confirm the cause before changing either side, then reuse one build. Record
  the Dart check's before/after time in the PR.
