# [S] moq-ffi's warmup test keeps every group under load

## Goal

moq-ffi's `media::read_tests::raw_media_catalog_warmup_reaches_the_subscription` passes under load
and runs again. The reader keeps the 4.7 s group that sits between the start the warmup reaches and
the live edge.

## Plan

Planned 2026-11-08 (maintainer's standing rule that flakes join this epic). The test failed in the
merge queue for [moq-dev/moq#8272](https://github.com/moq-dev/moq/pull/8272), which touches no
moq-ffi read path ([job 128797584818](https://github.com/moq-dev/moq/actions/runs/42502335110/job/128797584818)).
It panicked at `rs/moq-ffi/src/media.rs:1635` with `video=false; left: [4400000, 5000000]; right:
[4400000, 4700000, 5000000]`: the audio pass started at group 3 and ended at group 5, but skipped
group 4. It is quarantined with `#[ignore]` and a `sh/gh/test-inventory.allow` entry until this quest
fixes it.
Its subscription assertions moved to `raw_media_catalog_warmup_widens_the_subscription`,
which stays in the gate.

The test writes groups 5, 4, 3 newest first, finishes the track, then reads. The read polls on the
FFI runtime, whose clock is real time, so every wall-clock rule in the read path runs on a loaded
host's clock. Group 4 lands before group 3, so a rule keyed on arrival ages group 4 out first, while
group 5 survives as the newest and group 3 as the start. Candidates in `moq-mux`'s
`container::Consumer`: the missing-sequence walk (`front.late.elapsed() >= max_delay` and its hole
timer, with a 200 ms max delay), and the start's `Startup::Earliest` arrival anchor. Also check
`moq-net`'s subscription delivery under the widened 500 ms max delay.

Reproduce first: run the test repeatedly under a loaded `just check`, with tracing on
`skip_to` and the hole walk, to name the rule that drops group 4. Then fix the cause:

- If the rule drops an arrived, finished group whose media lies inside the budget, that is a
  `moq-mux` bug a real viewer on a slow host hits too. Fix it there, with a test on a controlled
  clock beside the start-reach tests.
- If the skip is correct for that much elapsed time, drive the test's clock instead of the host's
  (`kio::clock::Manual`, or build the consumer on the test runtime's clock), so elapsed time is
  zero by construction.

Then remove the `#[ignore]` and the allowlist entry, and delete this quest.

Public API: none. Wire: none.
