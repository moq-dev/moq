# [S] Cluster peers have one identity each and fresh inputs

## Goal

A relay mesh fails loud on identity mistakes instead of silently partitioning or looping, and its peer inputs stay current and bounded (`rs/moq-relay/src/cluster.rs`, `relay.rs`):

- A peer session whose declared hop (`Request::peer_hop`) equals our `cluster.id` is refused with an error naming the duplicate, instead of every route from it being dropped as reflected.
- One relay has one dial key: `DialTarget` keys on `canonicalize_peer_key` (which also strips URL userinfo, so credentials stop reaching logs and the `remote` span), and the self filter applies to static `cluster.connect` entries too. A test covers the self filter.
- `--cluster-lan` on a relay without a QUIC listener is refused at startup instead of advertising SRV port 0.
- The `cluster.token` file is re-read on redial (or watched like the API file), so a rotated or expiring token does not break every redial until restart.
- The `cluster.api.url` body and file are read with a size cap; an oversized answer keeps the last good peer list.

## Plan

Found by the 2026-11-07 invariant audit (relay-2, relay-4, relay-5, relay-7, relay-9, relay-10).
