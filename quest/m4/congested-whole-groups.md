# [L] A congested path sheds whole video groups, not each group's tail

## Goal

When a subscriber's path is narrower than a track, the publisher (or relay)
drops whole groups so the ones it sends arrive whole, instead of starting
every group and cutting each one short. A viewer then sees continuous video
broken by a skipped GOP, not a freeze in every GOP.

Today the scheduled `TS congested path` step (7 Mb/s, behind a 2 s queue,
under 9 Mb/s video and four audio PIDs, so the path carries about 73% of
the media) keeps only 42 to 47% of the video's frames. Each second the
export logs `current group evicted; skipping to next buffered group` and
resumes at the next group: every group plays its first 0.3 to 0.7 s, and
then the next group's start supersedes it, which is newest-first serving
under a 1 s max delay. Shedding whole groups at that rate would keep about
70%. The 2026-11-21 run kept 42% too (68 s of 117 s shed), so this
predates [moq-dev/moq#9316](https://github.com/moq-dev/moq/pull/9316).

## Plan

Measure first, at the relay and the publisher: which group streams are open
at once, how the congestion window is split between them, and when each is
reset. `just test ts --headroom --rate 7000000 --duration 20` with
`RUST_LOG=info,moq_net::lite=debug` and `HARNESS_KEEP=1` shows each served
group in the run's `relay.log`. Then change where the measurement points,
in Rust and `@moq/net` alike, and grade it with a `holes.py` bound on the
kept video share under `--shed`.

## Waiting

- Maintainer: which groups a narrow path keeps. (a) Newest first, as now:
  every group starts and loses its tail. (b) A started group keeps the path
  until it finishes or ages past the subscription's max, and a newer group
  that cannot also arrive inside the max is skipped whole. Recommended: (b),
  since a group is only decodable from its start, so a tail cut wastes the
  bandwidth that carried its start, and the max already says how long an
  older group stays worth sending.
