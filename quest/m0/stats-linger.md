# [XS] Stats broadcasts linger

## Goal

A grouped stats broadcast (`<prefix>/<group>/node/<node>`) stays announced for
a linger after its group's last session and traffic row leave, so a viewer
leaving and another arriving within it causes no unannounce and no new
announce across the mesh. The counters it reports, and any usage read from
them, are unchanged.

## Plan

Landed on `main` in [#4871](https://github.com/moq-dev/moq/pull/4871):
`moq_stats::produce::Config::linger` (default 5 minutes), surfaced in the
relay as `stats.linger` / `--stats-linger`. While a group lingers empty its
tracks read `{}`, and a path that left drops out of frames, so its return
reads as a restart.

Remaining: backport #4871 to `release` as an additive cherry-pick PR, since
moq.pro tracks `release` and its m0 stats-linger quest waits on a `release`
commit carrying it (decided 2026-10-05). `release`'s `produce.rs` differs
from `main`'s only by the wall-clock group seed of #4810, which keeps group
numbers increasing across a return. Both branches still have the per-path
maps, so the backport keeps #4871's choice: the path drops out of frames while the
group lingers, rather than the producer carrying its last totals forward.
Delete this quest once the backport merges.

## Related

- [Stats epochs](/quest/m0/broadcast-epoch/stats-epoch.md) - the name a lingering broadcast keeps
- [moq.pro: stats linger](https://github.com/moq-dev/moq.pro/blob/main/quest/m0/stats-linger.md) - the fleet adoption and its measurements
