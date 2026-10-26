# [M] A reported drop means the same in Rust and JS

## Goal

Rust and JS agree on whether a group reported as dropped can still be served
later, on the plain cursor and the resumed (routed) path alike.

## Plan

Found by [moq-dev/moq#6758](https://github.com/moq-dev/moq/pull/6758). JS
records a reported drop as delivered, so no copy serves that group later. In
Rust a plain cursor reports a fill it gave up on as `NotFound`, and the group
can still arrive and be served. Making Rust's drops final broke
`rejoin::rejoined_quiet_catalog_survives_cache_expiry`: a late reader never got
the catalog's group 0.

Decided 2026-10-25 (maintainer): a reported drop is not final, as in Rust.
JS stops recording a reported drop as delivered, so a group that arrives
later is still served, on the plain cursor and the resume path together,
with tests in both languages.

Public API: none expected. Wire: none.
