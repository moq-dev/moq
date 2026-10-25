# [XS] Nightly's base in a shallow checkout

## Goal

`test_paired_base_is_the_previous_scheduled_nightly`
(`sh/gh/nightly.test.py:46`) passes in Nightly's Tests matrix.

## Plan

The test runs `git rev-parse HEAD~1`, but the Tests matrix in `nightly.yml`
checks out with the default `fetch-depth: 1`, so it exits 128. Added by
[moq-dev/moq#6511](https://github.com/moq-dev/moq/pull/6511). Make the test
build its own history instead of reading the checkout's.

Public API: none. Wire: none.
