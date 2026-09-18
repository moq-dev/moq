# [XS] Delete moq_net::PathPrefixes

## Goal

`moq_net::PathPrefixes` (`rs/moq-net/src/path/mod.rs`, re-exported from
`lib.rs`) is gone. Origin scopes are `Patterns` now, and once the pattern
union PR lands nothing in the repository constructs a `PathPrefixes`. A public
type with no consumer is surface the release would have to keep.

## Plan

Delete the type, its re-export, and its tests; move any helper the origin
still uses onto `Patterns`. `just check` across the workspace finds any
straggler. Public API: breaking on moq-net, so on dev. Wire: none.

## Required

- PR #3746 has merged: it removes the last internal use, `PathPrefixes::from_patterns`
