# [XS] main builds on moq-noq 2.0.2

## Goal

`main` depends on moq-noq-proto, moq-noq-udp, moq-noq, and web-transport-moq
2.0.2 instead of 2.0.1, so it carries the same max datagram size fix
(moq-dev/noq#28) that `release` gets in 1.3.4.

## Plan

2.0.2 was published to crates.io on 2026-10-08. Bump the root `Cargo.toml`
pins and `Cargo.lock`, the same as #4699 did for 2.0.1. Kept in m0 (decided
2026-10-08) so `main` and `release` don't drift on the noq fix.
