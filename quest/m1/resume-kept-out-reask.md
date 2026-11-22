# [L] A resumed read asks upstream for a kept-out skip no copy still knows

## Goal

A bounded resumed read ends once every group of its window is read or
accounted for, even when a group the local floor or cap keeps out, under the
serving copy's resume point, was declared skipped only by copies the reader
has since let go. What the reader holds for this stays bounded by its gaps,
however many routes come and go: it asks upstream again instead of keeping
the history. Rust (`rs/moq-net/src/model/resume.rs`) and JS
(`js/net/src/resume.ts`) alike.

## Plan

Found on [moq-dev/moq#9326](https://github.com/moq-dev/moq/pull/9326).
That PR keeps a let-go copy's skips only at the start and end of each gap the
reader lacks (`Reader::skipped`, at most two runs a gap), merged across every
copy held or let go, because keeping every interior skip let the retained set
grow with route history while delivery stood still (Codex, round 5: a local
cap of `0..0` and repeated switches to copies with disjoint skips). The case
it leaves open (CodeRabbit on that PR): copy A alone declared `[2,3)` skipped
inside the lacking gap `[1,5)` with the local floor at 3, A is let go, and a
later copy declares `[1,2)` and `[3,5)`. Group 2 is then owed with no copy
left that knows it, and the read stays pending.

Recommended: when a kept-out key under the serving copy's resume point is
neither skipped by a held copy nor in `Reader::skipped`, ask upstream for it
as `Reader::backfill_gaps` asks for a gap the reader lacks, so the answer
(a skip, a drop, or the group) settles it, and delete what the retained set
no longer needs. This follows the repository's cache rule: a copy answers
from memory only what it can prove upstream would answer. Add the case above
as a unit test in both languages.

Decided by the maintainer: owe it, ask upstream. A key the local floor or
cap keeps out stays owed by a bounded read, as on one cursor, and the reader
asks the serving route for it, so a single cursor and a resumed reader end
the same read at the same time.

## Related

- [Resume model placement](/quest/m0/nightly/resume-model-placement.md) - the oracle owing a kept-out group below the serving placement
