# [XS] Bump web-transport-iroh for the capsule close

## Goal

Condition: [moq-dev/web-transport#419](https://github.com/moq-dev/web-transport/pull/419)
merges and ships in a `web-transport-iroh` release (check crates.io). Then the
root `Cargo.toml` names that release as the floor and `Cargo.lock` resolves it,
so moq's iroh HTTP/3 client reports a peer's close capsule as its code and
reason instead of `(0, "stream closed")`.

## Plan

Only the version moves; the fix and its regression test live upstream.
As of 2026-10-08 #419 has merged (2026-10-07), but the newest
`web-transport-iroh`, 0.8.2, was published hours before it, and `main`
floors 0.8.1. The release PR,
[moq-dev/web-transport#418](https://github.com/moq-dev/web-transport/pull/418),
cuts 0.8.3 with the fix. Advance it by merging #418, then floor 0.8.3.
