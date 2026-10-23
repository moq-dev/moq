# [M] A lite-06 copy that stopped at a request's end bound accepts its tail

## Goal

Over lite-06, a group received for a request whose end falls mid-group stays
open to the rest of that group. When later demand asks the same sequence past
that end, a second subscriber on the same relay reads the frames it asked for.
This holds for plain tracks; lite-06 has no compression.

## Plan

Lite-07 landed the receive side in
[moq-dev/moq#6447](https://github.com/moq-dev/moq/pull/6447): GROUP carries
`Frame End`, a FIN there suspends the cached copy
(`group::Producer::suspend`), and the stream continuing it takes the copy over
(`track::Producer::poll_resume_group`), waiting for the stream it overtook and
dropping a replayed prefix. Reuse that path; only the boundary signal differs.

Decided by the maintainer (2026-10-23): lite-06 GROUP has no such field and
stays unchanged, so a stream's bound comes from request correlation. Each end
revision inside a group gets its own request identity: the relay moves an end
frame with a fresh SUBSCRIBE instead of a SUBSCRIBE_UPDATE, so the request a
stream belongs to fixes its end. A FIN at that end suspends; below it ends the
group. The fresh request starts at the copy's resume frame, so a lite-06
publisher serves the tail with its existing `Frame Start` handling. Bound the
churn: only a move of the end frame within a group needs a new request, and
the replaced one is cancelled once its streams settle.

Reproduce first with the `bounded_tail` harness in
`rs/moq-net/tests/integration/bounded_tail.rs` on `moq-lite-06`; the original
failing case is in commit d221093adb. Cover an early real end, an end exactly
at the cap, repeated widening, and the replaced request's streams arriving
after the new one's.

Public API: none. Wire: none.

## Related

- [IETF bounded tails](/quest/m1/subscribe-ranges/bounded-tail-ietf.md) - the same continuation over moq-transport's existing end signal
