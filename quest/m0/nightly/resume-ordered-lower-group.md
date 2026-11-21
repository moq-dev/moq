# [M] A JS ordered reader reads a group its range edit brings in below one it read

## Goal

The Nightly `test resume-model` sweep's JS half passes on any seed: an
ordered JS reader whose range edit, or a route switch, brings in a group
below one it already read or skipped still delivers that group when its
range owes it, as the Rust reader does.

## Plan

Found on [moq-dev/moq#9278](https://github.com/moq-dev/moq/pull/9278)'s
sweeps (`MOQ_RESUME_SEED=10`, 4096 cases) and reproduced on `main`. Each
counterexample ends with the oracle's `missing groups 0`:

- No route switch:
  `join r3 ordered [0,1]; write 0/0 g1; write 0/0 next; ranges r3 [0,1]; advance 1`.
- `join r1 ordered [0,1]; write 0/0 g1; up 0/1; ranges r1 [0,1]; ranges r0 none`.
- `up 0/2; join r3 ordered [0,1]; write 0/2 g1; down 0/2; ranges r3 none; write 0/0 g0`.

The first one needs no resume at all, so start in the ordered reader's range
edit in `js/net/src/track.ts` before `resume.ts`. Decide from the Rust reader's
behavior whether the oracle or the JS reader is wrong; if it is the oracle,
fix both oracles together. Add each case to `regressions` in
`js/net/src/resume.model.test.ts`, and to the Rust model's when it reaches
code both share.

Public API: none. Wire: none.
