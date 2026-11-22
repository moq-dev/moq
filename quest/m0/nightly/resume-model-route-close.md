# [M] A group a closing route cuts short ends as the resume model expects

## Goal

The Nightly `test resume-model` sweep's Rust half passes on any seed: a
group whose serving route closes or fails mid-group ends the way the model
in `rs/moq-net/src/model/resume_model.rs` allows, or the model is corrected
where it is wrong.

## Plan

Found on [moq-dev/moq#9318](https://github.com/moq-dev/moq/pull/9318)'s
sweeps, once the drop cursor panic it fixed no longer masked them. Two
minimal cases at 4096 cases:

- `MOQ_RESUME_SEED=19`, also failing on `main` at `47099060ee`: `reader 0:
  group 5 ended cleanly after 2 frames`.

  ```text
  Join r0 (unordered, Hour, no range, no drops)
  Write 0/0 group 5, 2 frames
  Up 0/1
  Write 0/1 group 5, 1 frame
  Down 0/0
  Close 0/1 (no group, route stays up)
  ```

- `MOQ_RESUME_SEED=8`: `reader 0: group 5 ended after 1 frames where a
  failed route closed it`.

  ```text
  Up 0/2
  Up 0/1
  GiveUp 0/1 group 5
  Join r0 (unordered, Hour, no range, no drops)
  Write 0/2 group 5, 1 frame
  Close 0/2 (no group, route goes down)
  ```

First decide for each whether the implementation or the model is wrong: in
the second, the only route left gave up group 5, so ending it short may be
right and the model's `closed_at` check too strict. Add each case to
`regressions()` once it passes, with a unit test beside the fix.

Public API: none. Wire: none.
