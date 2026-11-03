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
