# [M] Relay ACME on instant-acme

## Goal

moq-relay's default `acme` feature issues and renews certificates through
instant-acme on `rind-instant-acme`, with no rustls-acme, and its graph holds
no aws-lc-rs. Existing config and the on-disk cache keep working.

## Plan

Decided by the maintainer on 2026-10-26: rustls-acme hard-wires ring or
aws-lc-rs for account keys, CSRs, and thumbprints, and has no provider hook
([FlorianUekermann/rustls-acme#62](https://github.com/FlorianUekermann/rustls-acme/issues/62)),
so moq-relay moves to instant-acme 0.9, which takes its crypto through its own
`CryptoProvider`. instant-acme is only the ACME client, so moq-relay now owns:

- the renewal loop that rustls-acme ran,
- the TLS-ALPN-01 challenge resolver in its rustls config,
- key and CSR generation through `rind-rcgen`.

The cache's file naming (SHA-256 today) moves to rind and stays compatible, so
an upgraded relay reuses its cached account and certificate. Test against a
local ACME server such as Pebble.

Public API: none; config unchanged. Wire: none.

## Required

- [rind runs on a Pi 4's aarch64 CPU](/quest/m2/rind/aarch64.md) - the rind release this switches to
- [rind-instant-acme is on crates.io](/quest/m2/rind/instant-acme.md) - the adapter this uses, waiting on instant-acme 0.9
