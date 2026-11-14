# m5: deferred

## Goal

Work that waits on something besides priority: either a first step outside
this repository (hardware nobody on the team has, a partner or customer who
asks for it, a hosting provider's offer, or an upstream release), or a
speculative feature or study with no named consumer yet.

## Plan

Decided 2026-11-13 (maintainer, /quest-plan): this milestone requires the
[release cut](/quest/m3/release/cut.md), so until it lands spare capacity goes
to m0 to m3 and then to [m9](/quest/m9/README.md)'s lab work (rejected:
requiring m0, m1, and m2, which lifts only when they empty, and m0 rarely
does; also gating m3, which holds the cut). The cut's PR deletes the cut
quest and points this gate at the next cycle's wire cut, which keeps it
every cycle; m5's quests start once the shift brings them into m4.

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

Pruned 2026-11-09 (maintainer, /quest-plan from the deletion audit): 80
quests with no named consumer, no outside gate, and no m0 to m4 quest
requiring them were deleted; git history keeps them. Consumers were checked
in this repository and in moq.pro's plans, which kept the io_uring tuning
quests (fleet relays on io_uring workers), broadcast fairness, and the
transcode and NVENC quests (production transcoding). Re-plan one when a
consumer asks.

Parked 2026-11-12 (maintainer, /quest-audit before the cut): 29 quests the
audit judged not worth their weight (no consumer in this repository or the
downstream plans, and no bug in shipped code) are kept here instead of
deleted (rejected: deleting them; leaving them in m3 and m4). Each says so in
its Plan. The tuning experiments that touch no published surface moved to
the lab, [m9](/quest/m9/README.md).

## Required

- [Release cut](/quest/m3/release/cut.md) - the cut lands before this milestone's work starts
