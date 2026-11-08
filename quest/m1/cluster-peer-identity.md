# [S] A cluster peer's identity is a type, not a string

## Goal

Relay mesh identity mistakes are unrepresentable or fail loud (`rs/moq-relay/src/cluster.rs`, `session.rs`):

- A `PeerKey` type, constructible only through `canonicalize_peer_key`, keys the dial map, the self filter, LAN discovery and `/nodes`. Today `DialTarget` keys on the raw URL with only the query cleared, so one relay can get two links, a node can dial itself, and URL userinfo reaches logs and the `remote` span. The self filter applies to static `cluster.connect` entries too, and a test covers it.
- A peer session whose declared hop (`Request::peer_hop`) equals our `cluster.id` is refused with an error naming the duplicate, instead of every route from it being dropped as reflected and the mesh silently partitioning.
- The `cluster.token` file is re-read on redial (or watched like the API file), so a rotated or expiring token does not break every redial until restart.
- A `session::Registry` registration removes only its own entry, so a reused `Request` id cannot unlist another live session.

## Plan

Found by the 2026-11-07 invariant audit (relay-2, relay-4, relay-5, relay-9, relay-13).

Decided 2026-11-07 (maintainer, invariant-audit root causes): make the canonical key a type so no code path can key on a raw URL again. The audit's suggestion of a byte cap on the `cluster.api.url` body is dropped: that endpoint is operator-trusted, and a constant cap is not a design bound.
