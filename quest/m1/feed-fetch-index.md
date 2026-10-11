# [S] Held feed fetches wake per group and are capped per track

## Goal

A relay serving fetches over a pre-lite-05 upstream (#5209) does work in
proportion to the groups that arrive or fold, not to how many fetches it
holds. A peer cannot grow the held set without bound: past a per-track cap,
a new held fetch is refused loudly. Lands on `main` first, then is
backported to `release`.

## Plan

Found by Codex security on #5210 (2026-10-10). `FeedFetches` in
`rs/moq-net/src/lite/subscriber.rs` keeps every held fetch in one queue and
rescans all of them on each wake. Each fetch needs a live caller and a
stream, and fails once the `Tail` ledger gives up on its group, but nothing
bounds the queue itself.

- Decided (2026-10-10): index held fetches by group, so an arrival or a fold
  wakes only the fetches waiting on that group, and the fold deadline stays at
  the earliest pending group.
- Decided: cap held fetches per track and refuse extras with an error, in
  line with the session's other resource caps. Pick the limit from the
  existing caps rather than inventing a new knob, unless one is clearly needed.
- Add a benchmark swept over held fetches and tracks, per AGENTS.md's fan-out
  rule, so a cost that grows with the held set shows up as a slope.
- Tests use a mocked clock: the cap refuses the next fetch, and an arrival
  wakes only its own group's fetches.

Public API: behavior only (a capped fetch is refused). Wire: none.

## Related

- [Resume reorder](/quest/m1/resume-reorder.md) - later moves held fetches onto the model's arrival record; keep the index easy to carry over
