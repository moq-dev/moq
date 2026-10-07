# [XS] A subgroup starting at object 0 is whole

## Goal

A draft-18+ subgroup stream with FIRST_OBJECT clear whose first Object ID
is 0 is read as a whole group, in `rs/moq-net`, instead of being dropped as
"a group with no head". A clear bit with any other first ID is still dropped
as it is today.

## Plan

Triaged from Fastly's moq-relay-interop report (2026-09-23 run, item 7) on
2026-10-07. d18 §2.2 says the original publisher MUST set FIRST_OBJECT when
it opens a subgroup, and a relay MUST keep it set, so the probe that left it
clear was the one out of spec. The first Object ID is absolute whatever
the bit says (§11.4.2), and object IDs in our model start at 0, so ID 0 is
the head either way.

Decided 2026-10-07: accept it anyway, to be lenient toward publishers that
are out of spec. Rejected: keeping the drop and documenting it.

Where it lives: `rs/moq-net/src/ietf/subscriber.rs` (around line 2611)
drops the stream before reading any object. Peek the first object's ID
before deciding. `next_object_id` already enforces the ID sequence. Check
whether `js/net` drops these streams at all; it has no matching check today.

Test: a d18 stream with FIRST_OBJECT clear and first ID 0 delivers its
group; one with first ID 3 is still dropped.

Public API: none. Wire: none.
