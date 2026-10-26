# [S] A declared fMP4 boundary opens a group only on a sync sample

## Goal

When a passthrough fMP4 import sees a declared segment boundary (`styp` or
`cut()`) whose fragment does not start with a sync sample, it does not open a
new group on the non-sync samples. Passthrough and unwrapped mode agree.

## Plan

Found by [moq-dev/moq#6743](https://github.com/moq-dev/moq/pull/6743):
unwrapped mode keeps those samples in the previous group, while passthrough
opens a new group a decoder cannot start from. Reproduce with a fixture, then
share one rule between both modes.

Public API: none. Wire: none.
