# [M] A reader resumes on a detour learned after its session closed

## Goal

A relay whose serving session closes before a detour to the same Instance
reaches it keeps its front, and its readers resume on the detour once it
arrives, every group once. Today the front ends at the close: the reader
restarts onto another Instance serving the path, or, when none does, reads
nothing more for good, even though the same Instance is back on the detour a
round trip later.

`route_audit::failures::a_relay_resumes_on_a_detour_learned_after_the_cut`
and the readers `detour_lost` pins in `rs/moq-net/tests/integration/route_audit/`
read on seamlessly, and `detour_lost` is gone.

## Plan

Found by [the uncovered cases](https://github.com/moq-dev/moq/pull/9111). It
happens wherever every neighbour's route to the announcing relay ran through
the relay that lost its session, so none was offered back to it: a ring of
four or more, or any mesh where the relay's neighbours reach the publisher
through it.

Root cause: `source_closed` excludes the closed source's route id
(`rs/moq-net/src/model/front.rs`, `self.excluded.insert(candidate.route)`).
A lite-07 claim keeps one table entry while the route layer moves it to
another next hop, so this excludes the very entry the detour revives. With no
other route at that instant, `pick` finds no candidate, and the front ends
for good (`select` in `rs/moq-net/src/model/origin.rs`, then
`end(Error::Unroutable)` in `front.rs`). A following application is then
stuck: `Following` hands out a closed broadcast again only once its Instance
stops serving the path (`rs/moq-net/src/model/follow.rs`, `moved`), and the
Instance never stops: it is back on the detour.

Recommended: a front rides through its claim's debounce. A closed source no
longer excludes a route-layer entry (`entry.node.is_some()`), or `select`
waits rather than ends while an excluded entry of the resolved Instance is
still shown unavailable. A track that nothing resumes still ends with the
session's error, not a later `Unroutable`. Rerun the `verify-failover` model,
since this changes the front lifecycle it covers.

An experiment with the first form (about 8 lines in `pick`) made every audit
case seamless and changed 7 existing tests: two read on where they expected
an end (`route_cluster::a_partitioned_lan_reaches_itself_through_the_cdn_and_heals`
and `catalog_handover::lite_07::reconnect_with_viewer`), and five ended a
debounce later with `Unroutable` instead of at once with the session's error
(`subscription_end_integrity::a_session_death_ends_the_track_with_its_error`,
`subscription_end_integrity::a_session_close_cancels_the_track_from_either_side`,
`track_tail::a_session_death_before_the_tail_aborts_the_track`, and both
`multi_cdn::transport_*_secondary_restarts`). The first two are the fix
working. The other five keep their error once the session's error is
carried to the end.

Rejected: letting `Following` hand out the same Instance again once its
broadcast ended while the Instance still serves the path, which removes the
permanent stall but restarts the reader at the live edge, skipping groups;
and accepting it, which leaves a reader stuck on a one-round-trip gap,
against requirement 2 of [the epic](/quest/m2/multicast/README.md).

Not a wire change.
