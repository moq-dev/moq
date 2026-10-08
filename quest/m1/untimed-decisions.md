# [XS] The maintainer decides two untimed follow-ups

## Goal

kixelated decides two questions [#4822](https://github.com/moq-dev/moq/pull/4822)
(the untimed model) raised, and each answer becomes a quest or is dropped.
Requested by an external consumer (OneTooMany), who leaves these calls to the
maintainer. This quest waits on that decision; delete it once both are
answered.

## Plan

1. **moq-archive and untimed tracks.** `Writer` refuses an untimed track with
   `Error::Untimed`, because the format stores a timestamp per frame. So
   `moq export archive` over moq-lite before 05, or moq-transport without
   TIMESCALE (every track on drafts 14-16), now fails where it used to record
   arrival times.
   - Recommended: keep refusing. Playback needs media time, and send or
     arrival times would mislead a reader. Revisit when someone needs to
     archive over those wire versions.
   - Alternative: bump the format so a frame's timestamp is optional, and
     replay an untimed track untimed.
2. **A malformed object in a FETCH.** On a TIMESCALE track, a subgroup object
   without a Timestamp ends the whole track with `MalformedTrack`
   (`rs/moq-net/src/ietf/subscriber.rs`, `recv_group`), as js/net does
   ([#4968](https://github.com/moq-dev/moq/pull/4968)), and so does one in a
   joining FETCH's fill (`recv_fill`). The same object in a standalone group FETCH, which a
   relay sends to fill a cache miss (`run_group_fetch`), fails only that group.
   - Recommended: end the track too. The publisher broke the track, however
     the object arrived.
   - Alternative: keep it to the group, so a one-off fetch response can't tear
     down a live subscription.

Settled while planning this (2026-10-07): MOQtail (HEAD `d642d335`) never sends
TIMESCALE as a track property; it puts the scale in its catalog JSON and stamps
objects with LOC's old Timestamp id 0x06. Its tracks arrive untimed, so the
malformed rule can't reach them, and nothing is left to decide there.

## Related

- [One max_age meaning](/quest/m1/cache-max-age.md) - the other untimed follow-up, already planned
- [FETCH_OK carries the track's properties](/quest/m1/fetch-ok-properties.md) - also touches how a FETCH learns timedness
