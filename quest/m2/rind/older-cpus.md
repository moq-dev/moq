# [XS] rind runs on CPUs without the crypto extensions

## Goal

A rind 0.1.x release on crates.io runs on aarch64 CPUs without the `aes`,
`pmull`, and `sha2` extensions (Raspberry Pi 4, CM4, Zero 2 W), and on x86_64
CPUs without AVX2, BMI2, ADX, or AES-NI (pre-2014 Intel, pre-Zen AMD,
Atom-class Celerons, QEMU's default CPU models). It holds once moq-dev/rind's
[aarch64 quest](https://github.com/moq-dev/rind/blob/main/quest/m1/moq/aarch64-portable.md)
and [x86_64 quest](https://github.com/moq-dev/rind/blob/main/quest/m1/moq/x86-portable.md)
are deleted and a release after both is on crates.io. This quest only waits;
delete it once that holds.

## Plan

Decided with the maintainer on 2026-10-31 (aarch64) and 2026-11-01 (x86_64),
so no host loses support. Every area whose crate runs inside moq-cli or
moq-relay waits on it (moq-cli links moq-relay, moq-auth, and moq-tokio),
since any of them calling rind on such a CPU would refuse to start. moq-e2ee
does not wait: no binary links it yet.

Public API: none. Wire: none.
