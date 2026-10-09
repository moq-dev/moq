# [XS] End of Group status on a marked stream

## Goal

`rs/moq-net` and `js/net` accept an End of Group status object (0x3) on a
subgroup stream whose header sets the END_OF_GROUP bit, and end the group
there, instead of refusing the stream. imquic sends both on d16, d18 and
d21, and moq-dev loses each group's last object today.

## Plan

Triaged from Fastly's follow-up on #5020 (#issuecomment-6046932019, runs of
2026-10-07 on `0f310a5`). d16 §10.4.2 says the bit only lets a subscriber
infer the group's end at FIN, and nothing forbids sending the status too.

Where it lives: the `IngestPhase::Status` arm in
`rs/moq-net/src/ietf/subscriber.rs` accepts 0x3 only when `!self.has_end` and otherwise
returns `Unsupported`, which aborts the local group producer (`recv_group`
then returns `Ok`, so the session is not stopped). A downstream reader only
gets the rest of the group if the model's fetch path recovers it. Accept the
status either way, as END_OF_TRACK already is. Check the matching status parse in `js/net/src/ietf/object.ts`.

Test: a d18 stream with the END_OF_GROUP bit, objects 0..4, then a 0x3
status at 5 delivers a finished group with five frames.

Public API: none. Wire: none.

## Related

- [JS papercuts](/quest/m1/papercuts-js.md) - edits the same status branch in `js/net/src/ietf/object.ts`; whichever lands second rebases
