# [S] Announce counters

## Goal

A relay's `/metrics` counts its announcement traffic per stats tier (customer
and cluster) and direction: starts, ends, and in-place updates, plus the
encoded bytes its announce streams carry. An operator can then tell announce
traffic apart from subscribe and stats traffic, which live cannot today.

## Plan

On moq.pro's live fleet (2026-09-25), 93% of relay egress was not media (about
15 MB/s), and it tracked mesh degree rather than customer load. No announce
series exists anywhere, so whether announces drive it is unknown. Subscriptions
are already counted (`moq_relay_subscriptions_opened_total` in
`rs/moq-relay/src/internal.rs`); add the announce counters beside it, with the
same tier labels.

- Count at the lite and IETF announce writers and readers in `rs/moq-net`, into
  the stats model the session already reports through, so the relay only
  exports them.
- Bytes are the encoded announce-stream bytes, not `announced_bytes` (which sums
  raw path lengths and stays invariant under compression). Otherwise the
  counters cannot show what [Announce compression](/quest/m1/announce-compression.md)
  or [Babel routing](/quest/m0/babel/README.md) save.
- Document the series in `doc/bin/relay/`.

Tests: a two-relay cluster with one publisher counts one start per hop on
publish, one update per reroute, and one end on unpublish, each on the right
tier.

## Related

- [Skip unchanged announce updates](/quest/m0/announce-update-dedupe.md) - the first saving these counters should show
- [Fleet announce panels](https://github.com/moq-dev/moq.pro/blob/main/quest/m0/announce-panels.md) - charts these on live and records the baseline
