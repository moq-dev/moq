# m5: deferred

## Goal

Work that waits on something besides priority: either a first step outside
this repository (hardware nobody on the team has, a partner or customer who
asks for it, a hosting provider's offer, or an upstream release), or a
speculative feature or study with no named consumer yet.

## Plan

A quest gated on the outside world has a condition quest beside it, as the
root [questline](/quest/README.md) describes. A speculative feature or study
with no consumer needs no condition quest: it parks here rather than in m3 or m4
until a consumer appears, and is deleted when it goes stale; git history keeps
it. Decided in the 2026-10-05 audit: both kinds live here, and the m2 quests
deferred for having no named consumer moved here. The 2026-10-08 audit folded
m4's upstream waits in here and moved m2 work with no consumer.

Decided 2026-11-05 (maintainer, milestone re-cut): this milestone was m3 and
is now m5, after m3 and m4 took the two halves of the old m2. The old m2's
features, studies, and optimizations with no named consumer moved here, per
the rule above, unless an m0 to m4 quest requires them. It stays the deferred
bucket when milestones shift after a cut.

Rate-grant work targets the next unpublished lite version after finalized
lite-07; bitrate enforcement is not part of the next release.
