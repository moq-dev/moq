# [XS] release builds on moq-noq 1.3.4

## Goal

`release` depends on moq-noq-proto, moq-noq-udp, moq-noq, and
web-transport-moq 1.3.4 instead of 1.3.3, so a raw QUIC peer without
datagram support gets a max datagram size of 0 instead of a panic
(moq-dev/noq#29).

## Plan

1.3.4 was published to crates.io on 2026-10-08. Bump the root `Cargo.toml`
pins and `Cargo.lock`, the same as #4702 did for 1.3.3. PR targets `release`.
