# [XS] rind-instant-acme is on crates.io

## Goal

`rind-instant-acme` is on crates.io, built on a published instant-acme 0.9.
It holds once moq-dev/rind's
[Publish rind-instant-acme](https://github.com/moq-dev/rind/blob/main/quest/m1/moq/instant-acme-publish.md)
quest is deleted and `cargo search rind-instant-acme` shows it. This quest
only waits; delete it once that holds.

## Plan

rind's adapter waits on instant-acme's 0.9 release, which carries the
pluggable `CryptoProvider`. Only [relay ACME](/quest/m2/rind/acme.md) needs
it (maintainer, 2026-10-31).
