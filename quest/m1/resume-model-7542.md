# [S] The resume model passes its long run again

## Goal

`moq-net`'s route-switching resume model
(`model::resume_model::readers_match_the_logical_track`) passes at 3000 cases
on every seed again, fixed at its cause, so a reader with a long budget never
gets groups back as dropped that its route can still deliver.

## Plan

Planned 2026-11-02 (maintainer, follow-up planning after the 2026-11-02 /quest-complete run), from [moq-dev/moq#7625](https://github.com/moq-dev/moq/pull/7625)'s
checks.

- It fails on `main` on seeds 12345, 777 and 4242 with the same minimal cases,
  and bisects to [moq-dev/moq#7542](https://github.com/moq-dev/moq/pull/7542)
  (lite-07 frame bounds, timed datagram admission, sparse reverse search, and
  shared time-anchor walks). The 64-case run inside `just check` still passes.
- Symptom: a reader with an hour budget gets groups 0 to 10 back as dropped
  when the route only wrote group 11.
- Repro: `MOQ_RESUME_SEED=12345 MOQ_RESUME_CASES=3000 cargo test -p moq-net
  --all-features --release --lib model::resume_model::readers_match_the_logical_track`.
- Keep #7542's flat per-range costs; add the minimal case as a fixed regression.
- Also replay the clean-end stall that
  [moq-dev/moq#7739](https://github.com/moq-dev/moq/pull/7739) found and
  attributed to the fill-answer wake bug here (noted 2026-11-03, maintainer,
  /quest-plan of #7739's follow-ups): `MOQ_RESUME_SEED=17893651
  MOQ_RESUME_CASES=4096`. Shrunk case: Up instance 1/slot 2, Write group 11,
  Join arrival reader (Hour, default ranges, drop polling), End that route,
  finish=true. It hands out group 11 and drops 0..10, then never reports the
  end. Seed 1's regression (`Up i1s0, Join, Write g11, End i1s0`) is similar
  but differs in slot and order, so add this one too if it still fails.
