# [XS] Waiting: rust2 publishes its UniFFI fork

## Goal

[moq-dev/rust2](https://github.com/moq-dev/rust2) has merged its
[UniFFI import quest](https://github.com/moq-dev/rust2/blob/main/quest/m1/uniffi.md)
and published `rust2-uniffi` and `rust2-uniffi-bindgen` to crates.io, with a
tag moq can pin as a flake input. Check: `cargo search rust2-uniffi` lists a
release, and rust2's `quest/m1/uniffi.md` is gone from its `main`. Delete this
quest once both hold.

## Plan

Decided with the maintainer on 2026-10-23: the UniFFI hard fork and moq's
generator code live in rust2. This quest stands in for that cross-repository
blocker so [the switch](/quest/m2/uniffi-fork.md) resurfaces when it clears.
