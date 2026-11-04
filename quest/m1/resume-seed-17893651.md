# [XS] The resume model ends #7739's clean-end seed

## Goal

`moq-net`'s route-switching resume model
(`model::resume_model::readers_match_the_logical_track`) passes on seed
17893651, the clean-end stall that
[moq-dev/moq#7739](https://github.com/moq-dev/moq/pull/7739) found: a
long-budget reader whose only route ends gets the end, not a stall after
group 11.

## Plan

Noted 2026-11-03 (maintainer, /quest-plan of #7739's follow-ups) on the
resume-model quest that
[moq-dev/moq#7705](https://github.com/moq-dev/moq/pull/7705) finished.
#7705 fixed the fill-answer wake bug #7739 blamed for this stall, but its
checks ran seeds 1 to 16, 777, 4242, 12345 and the default, not this one.
Moved here on 2026-11-03 when merging `main` into
[moq-dev/moq#7747](https://github.com/moq-dev/moq/pull/7747) deleted that
quest.

- Repro: `MOQ_RESUME_SEED=17893651 MOQ_RESUME_CASES=4096 cargo test -p moq-net
  --all-features --release --lib model::resume_model::readers_match_the_logical_track`.
- Shrunk case: Up instance 1/slot 2, Write group 11, Join arrival reader
  (Hour, default ranges, drop polling), End that route, finish=true. It
  handed out group 11 and dropped 0..10, then never reported the end.
- If it passes on `main`, delete this quest. If it still fails, fix it at its
  cause and add the shrunk case to `resume_model::regressions`: seed 1's
  regression (`Up i1s0, Join, Write g11, End i1s0`) is similar but differs in
  slot and order.

Public API: none. Wire: none.
