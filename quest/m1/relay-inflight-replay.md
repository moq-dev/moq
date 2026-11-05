# [S] Legacy relay joins start at the answer's live edge

## Goal

A lite-03/04 SUBSCRIBE without a start, served by a Rust or JS relay from a
copy its upstream feeds, starts at the newest group the upstream's answer
brings, never at history the relay's cache or feed holds below it.

## Plan

Decided 2026-11-05 (maintainer, /quest-unblock), replacing the 2026-11-03
request-time floor and the 2026-11-04 copyless-front decision where they
conflict: ✅ rethink after the fourth review round's design checkpoint. Not
chosen: continuing the shared immutable snapshot design with targeted fixes.

- A pending join's start comes from the upstream answer's latest group,
  never from the relay's cache. Drop the snapshot machinery this makes
  unnecessary.
- An answer that carries no largest group waits about 50 ms past the answer
  and starts from the newest group received by then. Lite-03/04's
  SUBSCRIBE_OK carries none (Rust sends no `start_group`; JS echoes the
  requested one), nor do lite-01..06 answers. Mock time in its tests.
- Legacy lite-03/04 may keep minor known bugs; the PR body lists them.

Implemented: the publisher settles a lite-03/04 join that found no newest
group on arrival when a wire subscription feeds its track (a received copy,
or a front whose serving copy is one), then floors at the newest group known,
which includes a largest a lite-07 or moq-transport answer reported. Local
tracks and lite-05 take the newest group once the track resolves (lite-05's
first-frame latency allows no wait); lite-01/02 keep their arrival floor
(the 2026-11-04 parity decision still holds). Remaining: the restarted local
review rounds (`quest review --new`).
