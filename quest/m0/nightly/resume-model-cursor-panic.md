# [M] The Rust resume model sweep never panics in a drop cursor

## Goal

The Nightly `test resume-model` sweep's Rust half passes on any seed: no
sequence of writes, route changes, give-ups, and range edits panics a
track's drop cursor. A publisher or route that misbehaves must never panic.

## Plan

Found on [moq-dev/moq#9278](https://github.com/moq-dev/moq/pull/9278)'s
sweeps, and reproduced on `main` at `b4496b1207`: about one seed in four at
4096 cases (`MOQ_RESUME_SEED` 2, 8, 9, 11, 14, 16) panics with
`range start is greater than range end in BTreeMap` from `DropCursor::next`
in `rs/moq-net/src/model/track.rs`. A temporary assert showed
`Cover::gained` handing `state.lookup.range(..)` a reversed run (`9..2`,
span `9..MAX`) from `grown()`.

The minimal case, to add to `regressions()` in
`rs/moq-net/src/model/resume_model.rs` once it passes:

```text
Join r0 (unordered, Hour, no range, no drops)
Write 0/0 group 9, 1 frame
Up 0/2
Join r1 (ordered, Hour, range 7..=11, no drops)
Write 0/2 next group, 1 frame
GiveUp 0/2 group 1
Ranges r1 None
```

Find where the reversed run is produced rather than clamping it at the
lookup. Add a unit test in `track.rs` beside the model regression.

Public API: none. Wire: none.
