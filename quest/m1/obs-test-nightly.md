# [XS] Nightly runs the OBS TSan suite

## Goal

A Nightly job runs `just obs test` (the OBS plugin's ThreadSanitizer suite)
and its failure is visible like any other Nightly lane.

## Plan

Nothing ran it, so it failed every time on `main` unnoticed until
[moq-dev/moq#6277](https://github.com/moq-dev/moq/pull/6277) fixed its
suppression rule. Add it to the Nightly recipe matrix in
`.github/workflows/nightly.yml` with the `.#obs` shell it needs, and record its
runtime; a loaded TSan run of `moq-output-test` can take 60 to 90 s instead of
about 8.

Public API: none. Wire: none.
