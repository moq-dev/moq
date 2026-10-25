# [XS] Advisory bumps

## Goal

Nightly's Audit and JS audit jobs pass on the advisories open on 2026-10-24.

## Plan

- RUSTSEC-2026-0387: `crypto-bigint` 0.7.5 to at least 0.7.6.
- GHSA-xjj6-f33q-929c (high), GHSA-4qv8-2p9j-2685, GHSA-f72v-6w45-q472:
  `undici` 7.29.1 to at least 7.30.1, reached through
  `@moq/demo-carla > wrangler > miniflare`.

New advisories keep arriving, so the audits fail again on their own; the
standing review belongs to the [dependency audit](/quest/m2/audit-deps.md).

Public API: none. Wire: none.
