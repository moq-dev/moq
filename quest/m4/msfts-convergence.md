# [M] MPEG-TS lane converges with MSFTS ES-level carriage

## Goal

A subscriber author can map this repository's demultiplexed TS lane
(access units, Hang catalog `mpegts` section) onto MSFTS ES-level carriage
without guessing. #3731 settled four of six divergences (scope, group
alignment as a SHOULD, mux rate, egress timing); the payload unit (access units
versus filtered 188-octet packets) is MSFTS's call at msfts#33, and SI
repetition is now per `table_id` here versus a subscriber obligation there.
Transporting TS verbatim is a non-goal.

## Plan

Once msfts#33 settles, re-read the draft and decide what converges: a
published mapping from the `mpegts` catalog section to the `m2ts` fields, or a
change on either side. Update `drafts/draft-lcurley-moq-mpegts.md` and
`doc/concept` with whatever lands.

## Required

- msfts#33 (https://github.com/mondain/msfts/issues/33) settles the ES-level payload unit

## Closes

- [#3731](https://github.com/moq-dev/moq/issues/3731) - close this issue when the quest finishes
