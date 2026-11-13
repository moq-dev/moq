# [XS] moq-congestion's SCReAMv2 follows its draft and reference

## Goal

moq-congestion's `scream` carries every fix to its queue-delay target path
from newer revisions of draft-ietf-ccwg-rfc8298bis-screamv2 and from
[EricssonResearch/scream](https://github.com/EricssonResearch/scream)'s
`ScreamV2Tx.cpp`, each ported with its test or recorded as not applying, and
`rs/moq-congestion/src/scream.rs`'s module doc names the draft revision and
reference commit checked.

## Plan

Recurring child of the [vendor sync](/quest/m3/vendor-sync/README.md), which
says how each run works and recurs. New in the 2026-11-05 scan.

[moq-dev/moq#7547](https://github.com/moq-dev/moq/pull/7547) (merged
2026-11-03) ported draft
[-01](https://www.ietf.org/archive/id/draft-ietf-ccwg-rfc8298bis-screamv2-01.html)
and checked it against the reference without recording a commit. The
reference's `master` head then was
[`4b973bab`](https://github.com/EricssonResearch/scream/commit/4b973babf6555eda56c0d86649f0d16161813cb3)
(2026-10-27); start there, after confirming the port's constants match it.

Only the queue-delay target path is ported. The L4S path, competing-flow
compensation, and the draft's media-queue terms were left out on purpose (see
the [realtime uplink epic](/quest/m4/quic-realtime.md)), so a fix
confined to them does not apply. Keep the PR's two recorded deviations
(delay backoff counts as congestion; `scl_t` taken after the reduction) unless
upstream settles them differently. SCReAMv2 is a bake-off candidate; if the
bake-off deletes it, drop this child.

## Required

- [Cut and publish the prepared breaking release](/quest/m3/release/cut.md) - the next cycle starts after the cut
