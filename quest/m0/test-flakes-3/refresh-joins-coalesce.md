# [S] refresh_joins_coalesce_upstream passes in parallel runs

## Goal

`refresh_joins_coalesce_upstream` in `rs/moq-net/src/lite/subscriber.rs`
passes in every full parallel test run, fixed at its cause. Found on
2026-11-23 while kixel-ai/dotfiles measured linkers for moq builds
([kixel-ai/dotfiles#278](https://github.com/kixel-ai/dotfiles/pull/278)): it
failed 3 of 6 full runs linked with wild, 1 of 2 with mold and 0 of 6 with
rust-lld, and the replay seed it printed did not reproduce it.

## Plan

- Leading suspect: the join tick is process-wide. `JOIN_TICK` in
  `rs/moq-net/src/model/track/demand.rs` is a `static`, and the test steps it
  with `answer_elsewhere()` to make its 8 joins wait past different ticks.
  Any test on another thread that records a live join's answer moves the
  same counter between the test's steps, which a seed replay cannot
  reproduce and which would explain why it fails only in full runs. The
  linker split is likely timing noise.
- Reproduce first: run the test alone while another thread calls
  `demand::answered()` in a loop, and in a loop under `cargo nextest` or
  `just check` with the whole crate.
- If foreign ticks break it, decide which side is wrong. In production many
  tracks answer joins concurrently, so if coalescing sends extra
  SUBSCRIBE_UPDATEs when ticks interleave, that is a bug in the code, not
  the test: fix it there. If coalescing is right and only the test's
  expected `wanted_group` list assumes no foreign ticks, assert on what the
  joins must produce whatever other ticks interleave.
- If foreign ticks don't break it, look for another shared state or a real
  race, and check the JS mirror in `js/net/src/lite/subscriber.test.ts`.

## Hosts

- linux - reproduced in full parallel runs on fedora
