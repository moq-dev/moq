# [S] Re-run the per-layer crate split decision

## Goal

The maintainer re-decides whether moq-ffi splits into one UniFFI crate per layer,
so a layer reuses the net noun it wraps (`media.GroupConsumer(group:
GroupConsumer)`), from a fresh report against rust2's UniFFI fork.

## Plan

[moq-dev/moq#5460](https://github.com/moq-dev/moq/pull/5460) rejected the split
on 2026-10-13 because UniFFI keyed types by bare name, so no generator could
take the same noun from two crates.
[kixelated/rust2#21](https://github.com/kixelated/rust2/pull/21) fixes that in
every generator of rust2's fork, reusing #5460's two-crate harness. What #5460
measured still stands until re-measured: each extra crate adds about 48 KB of
scaffolding to the cdylib.

Public API: none here; a split would rename every binding's namespaces.
Wire: none.

## Related

- [Switch to rust2's UniFFI](/quest/m2/uniffi-fork.md) - the fork the split would build on
