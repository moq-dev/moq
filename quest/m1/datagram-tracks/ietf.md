# [S] moq-transport datagram tracks

## Goal

Over moq-transport, Rust and JS map a datagram track to IETF SUBSCRIBE and
OBJECT_DATAGRAM, with its sequence as the group ID and object ID 0, and
keep the requested kind when a peer mixes datagrams and streams on one track.

## Plan

Implementation checkpoint: [moq-dev/moq#7959](https://github.com/moq-dev/moq/pull/7959).
Rust and JS adapters, direct delivery, interop lanes, and mixed-kind filtering are
implemented. Full interop passed. The remaining blocker is upstream subscription
sharing: a relay currently opens separate stream and datagram subscriptions for
one IETF name. The extra subscription broadens the stream's demand and breaks
ordering, range updates, and max-age aggregation. Share upstream interest between
the model kinds without matching errors or adding IETF-only public model API,
then rerun the complete net suite, CI, interop, and local review.

The failing integration tests are `group_order::merged_subscribers_keep_their_own_order`,
`subscribe_ranges::{draft18_updates_narrow_then_widen,draft22_cold_widening_finishes_the_largest_group,draft22_resolves_cached_time_ends,draft22_updates_both_bounds_across_cached_groups,unresolved_time_reader_never_changes_sibling_demand}`,
and `subscriber_max_age::relay_forwards_the_most_tolerant_subscriber_max_age`.


Decided 2026-11-01 (maintainer), in the
[epic](/quest/m1/datagram-tracks/README.md):

- A datagram-track subscription keeps only datagrams with object ID 0.
  Stream objects arriving on it are dropped and logged once per
  subscription.
- A stream-track subscription drops datagrams.
- Outbound, a datagram track sends only OBJECT_DATAGRAM with object ID 0.
  A stream track never sends OBJECT_DATAGRAM.
- A datagram with a nonzero object ID on a datagram-track subscription is
  dropped and logged.

Touches `rs/moq-net/src/ietf/datagram.rs` and the JS IETF adapter. Test
against moxygen in the interop lanes where it sends datagrams.

Rust datagram tracks ([moq-dev/moq#7752](https://github.com/moq-dev/moq/pull/7752))
left moq-transport without datagrams in Rust until this lands: the IETF
subscriber drops every OBJECT_DATAGRAM, `datagrams()` on an IETF source is
NOT_FOUND, and a stream track never sends one. Restore what it removed:

- the Rust-JS `datagram` tail lanes in `test/interop/interop.sh`, the
  `datagram` mode of `test/interop/clients/js-native/tail.ts`, and their
  README bullet;
- moq-e2ee's `datagrams_over_ietf` transport test;
- the moq-transport rows of moq-tokio's `datagram_delivery` bench and
  moq-net's `session_delivery_datagram`;
- the OBJECT_DATAGRAM sentence in `doc/concept/standard.md`.

An OBJECT_DATAGRAM whose Object Status is undefined is a protocol violation
in JS too, as it is in Rust, on every subscription kind. Planned 2026-11-04
with the maintainer, from
[moq-dev/moq#7846](https://github.com/moq-dev/moq/pull/7846). Rust's
`recv_datagram` (`rs/moq-net/src/ietf/subscriber.rs`, about line 4839)
closes the session with `ProtocolViolation` when `status_defined`
(`rs/moq-net/src/ietf/object.rs`, about line 21: Normal, END_OF_GROUP and
END_OF_TRACK) rejects the status, even while it drops the datagram. JS's
`#recvDatagram` (`js/net/src/ietf/subscriber.ts`) decodes and drops it
silently, on main and after #7846. Check the shared status set once for
both datagram paths, so a datagram-track subscription keeps the check
when it starts accepting datagrams, and add a JS test that an undefined
status closes the session.

Public API: none beyond the model. Wire: no format change.
