# [S] Swapped Rust publisher to JS Opus in interop

## Goal

The `rust-swapped -> js` interop cell stops timing out on its Opus check
under local load, fixed at its cause.

## Plan

Seen in [moq-dev/moq#6745](https://github.com/moq-dev/moq/pull/6745)'s
`just test interop --all` runs: one of two runs timed out on this cell, and
the other passed. Reproduce under a loaded machine and find whether the JS
subscriber never receives the Opus track, receives it late, or decodes it and
misses the check.

Public API: none. Wire: none.
