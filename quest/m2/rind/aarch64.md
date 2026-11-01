# [XS] rind runs on a Pi 4's aarch64 CPU

## Goal

A rind 0.1.x release on crates.io runs on aarch64 CPUs without the `aes`,
`pmull`, and `sha2` extensions, such as the Raspberry Pi 4, CM4, and Zero 2 W.
It holds once moq-dev/rind's
[aarch64 quest](https://github.com/moq-dev/rind/blob/main/quest/m1/moq/aarch64-portable.md)
is deleted and a release after it is on crates.io. This quest only waits;
delete it once that holds.

## Plan

Decided with the maintainer on 2026-10-31, so no board loses support. Every
area whose crate runs inside moq-cli or moq-relay waits on it (moq-cli links
moq-relay, moq-auth, and moq-tokio), since any of them calling rind on such a
board would refuse to start. moq-e2ee does not wait: no binary links it yet.

Public API: none. Wire: none.
