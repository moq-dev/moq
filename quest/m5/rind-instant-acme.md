# [XS] rind-instant-acme is on crates.io

## Goal

`rind-instant-acme` is on crates.io, built on a published instant-acme 0.9.
This quest only waits; delete it once that holds.

## Plan

rind's adapter waits on instant-acme's 0.9 release, which carries the
pluggable `CryptoProvider`. Only [relay ACME](/quest/m5/relay-acme.md) needs
it (maintainer, 2026-10-31).

## Waiting

- moq-dev/rind: its
  [Publish rind-instant-acme](https://github.com/moq-dev/rind/blob/main/quest/m3/instant-acme-publish.md)
  quest is deleted and `cargo search rind-instant-acme` shows the crate.
  Checked 2026-11-07: the rind quest is still open, crates.io has no
  `rind-instant-acme`, and instant-acme's latest is 0.8.5.
