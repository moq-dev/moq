# [M] Bindings request and follow

## Goal

moq-ffi, the C ABI, and every wrapper (py, swift, kt, dart, go, cpp, OBS)
mirror `request` and `follow`, each with an optional epoch, and their
consumers use them.

## Plan

Split from [Two broadcast resolutions](/quest/m1/api-audit/resolve/README.md)
on 2026-10-28 (maintainer). Today's binding names already inherit the
owned-discovery behavior from
[moq-dev/moq#6445](https://github.com/moq-dev/moq/pull/6445), not the final
naming.

Regenerate bindings from source, update every wrapper's tests, READMEs and
`doc/lib/*`, add an Impact migration note per package, and run
`just test interop --all`.

Public API: breaking in moq-ffi, the C ABI, and every wrapper. Wire: none.

## Required

- [Rust request and follow](/quest/m1/api-audit/resolve/rust.md) - the Rust names the bindings mirror
