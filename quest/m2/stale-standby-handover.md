# [M] Re-promoting a demoted source copy stalls the whole chain

## Goal

A relay whose front moves back onto a source it left less than the idle
linger ago keeps delivering: the demoted copy's subscription is either released
when it stops being read or drained while it waits, so its receive window never
fills with groups nobody consumes, and the chain upstream of it never blocks on
that window.

## Plan

Seen with measured link costs (`[cluster.cost]`) on the three-relay triangle in
`rs/moq-relay/tests/cluster_testbed.rs`: a subscriber attaches while the first
announcements still travel over the link that came up first, so the front at
the last relay splices source A (via a middle relay), then source B (direct)
takes over 300 ms later when its cheaper price lands. A's subscription stays
open, unread, for up to `TRACK_IDLE_LINGER` (30 s). When B's measured price
climbs and the front moves back onto A, A's copy has groups queued behind a
full receive window that the front never reads (it wants the live edge), the
middle relay's publisher blocks on that window, its ingest stops pulling, and so
does the source relay's, until the publishing client sends nothing at all.
Every link goes idle while every session stays up and no error is logged.

Reproduce by removing the 5 s settle before the clients attach in
`lossy_direct_edge_is_routed_around` (`rs/moq-relay/tests/cluster_cost.rs`).
With the settle the switch subscribes A fresh and the stream continues.

Decide between releasing a demoted copy's upstream subscription as soon as the
front stops reading it (re-subscribing on re-promotion costs one round trip)
and keeping it but skipping its stale groups so the window drains. Either way a
regression test that attaches during the shuffle and asserts frames keep
arriving after the second switch.

## Related

- [PoP skipping](/quest/m2/pop-skipping/README.md) - every warm handover between two carrying relays is this switch
