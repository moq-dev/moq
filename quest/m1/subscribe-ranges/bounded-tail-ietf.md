# [M] A moq-transport copy that stopped at a request's end bound accepts its tail

## Goal

Over moq-transport drafts 20, 21, and 22, a group received for a request whose
end falls mid-group stays open to the rest of that group. When later demand
asks the same sequence past that end, a second subscriber on the same relay
reads the frames it asked for.

## Plan

Lite-07 landed the receive side in
[moq-dev/moq#6447](https://github.com/moq-dev/moq/pull/6447): a bounded FIN
suspends the cached copy (`group::Producer::suspend`), and the stream
continuing it takes the copy over (`track::Producer::poll_resume_group`),
waiting for the stream it overtook and dropping a replayed prefix. Reuse that
path; only the boundary signal differs.

Decided by the maintainer (2026-10-23): reuse `GroupFlags.has_end` and
END_OF_GROUP, with no new wire. The IETF publisher already clears `has_end`
when serving a bounded slice, so a bare FIN on such a stream suspends and an
END_OF_GROUP finishes. A receiver-only change is not enough: a local source's
genuine early end emits no explicit END_OF_GROUP today, so the publisher must
send one whenever the header did not promise the end.

`GroupIngest` collapses a bare FIN and END_OF_GROUP into `Ended::Group`; split
them. IETF readers get relayed groups whole because Object IDs may not be frame
ordinals (`track::Subscriber::deliver` skips the frame window when
`group.relayed()`), so continuing a copy needs the Object-ID to ordinal
mapping to agree across the two streams, including sparse IDs.

Reproduce first with the `bounded_tail` harness in
`rs/moq-net/tests/integration/bounded_tail.rs` on `moq-transport-20`, `21`, and
`22`; the original failing cases are in commit d221093adb, whose readers also
narrow their frame cursor so a stale prefix cannot hide the missing tail.

Public API: none. Wire: none.

## Related

- [Lite-06 bounded tails](/quest/m1/subscribe-ranges/bounded-tail-lite06.md) - the same continuation over lite-06 request correlation
- [moq-transport ranges](/quest/m1/subscribe-ranges/ietf.md) - carries a live range change upstream on REQUEST_UPDATE
