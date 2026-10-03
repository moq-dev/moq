# [M] A front's single-reader cost

## Goal

Reading a track through an origin front costs about what reading it directly
does for a single reader. Since [#4741](https://github.com/moq-dev/moq/pull/4741)
every track a front serves is written by a pump, one task hop behind the route,
which measured 1.5 to 2x slower than the splicer for 1 track x 1 reader (and 2.5
to 4x faster at 100 readers).

## Plan

Decided in planning (2026-10-03): measure first. Bench 1 track x 1 reader and
100 tracks x 1 reader through a front against reading the route's track
directly (`cargo bench -p moq-net --bench origin -- origin/relay`), then compare
adopting a whole finished group from the route with batched forwarding. A
measured no-win abandons the quest.

Sharing an open route-owned group shares its failure state with the route, so
only finished groups are candidates for adoption.
