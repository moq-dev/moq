# [M] Relay metrics through prometheus-client

## Goal

The relay's `/metrics` endpoint renders through `prometheus-client`, the
Prometheus project's maintained Rust client, instead of hand-written text
exposition, with the same metric names, labels, and types.

## Plan

Found by the 2026-10-19 dependency audit; adding the dependency is the
maintainer's call. `render_metrics` in `rs/moq-relay/src/internal.rs` writes
its metric families by hand (about 430 lines, histograms included), and
`rs/moq-relay/src/stun.rs` adds more. [Metric-safe tiers](/quest/m1/metric-tier.md)
adds label escaping by hand first.

`prometheus-client` 0.25.1 (released 2026-09) needs only `dtoa`, `itoa`,
`parking_lot`, and its derive crate without the protobuf feature. Keep the
snapshot model: build the registry from the `moq_net::stats::Snapshot` on
each scrape, or collect through its `Collector` trait, rather than adding
live counters to the hot path. Pin the current output with a golden test
before switching, so a renamed or retyped series fails loudly;
`doc/bin/relay/http.md` documents the endpoint.

A no-go is valid if the crate cannot reproduce the existing series without
renames.
