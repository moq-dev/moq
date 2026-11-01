# MoQ on rind

## Goal

Every MoQ crate takes its crypto from [rind](https://github.com/moq-dev/rind),
a pure-Rust fork of graviola, so no build compiles C crypto: on every release
target, including armv7 Android, `cargo tree -e normal,build -i` finds no
aws-lc-sys, ring, or `cc` in a crypto crate's graph. rind is MoQ's only crypto,
replacing aws-lc-rs, ring, and the RustCrypto `hmac`/`sha1`/`sha2`/`p256`/`p384`
crates in MoQ's own code.

Non-goals: CPUs or targets rind refuses (see Plan), and moq-tokio's `iroh`
feature, which is deleted instead.

## Plan

Decided with the maintainer on 2026-10-26 (planning interview). This
supersedes the m3 pure-crypto epic, which only moved moq-relay to graviola once
upstream graviola and ACME releases made it cheap. rind owns those gaps
instead, so every crate drops C, not just the relay.

- rind lives at moq-dev/rind, a GitHub fork of ctz/graviola with moq's repo
  setup. Its own quests live in its `quest/` tree; its
  [MoQ coverage epic](https://github.com/moq-dev/rind/blob/main/quest/m1/moq/README.md)
  carries rind's plan.
- MoQ uses rind's own API plus its adapter crates (`rind-rustls`,
  `rind-jsonwebtoken`, ...), never a ring facade through `[patch.crates-io]`,
  so a published crate built with `cargo install` also compiles no C.
- Unsupported is refused, never a fallback, and one backend compiles per
  arch: rind runs on x86_64 and aarch64 with graviola's CPU features, and on
  armv7 through libcrux. A host missing a CPU feature fails at startup with an
  error naming it. wasm32 and other targets do not build rind; nothing in MoQ
  needs native crypto there.
- Raspberry Pi 4, CM4, and Zero 2 W lack aarch64 crypto extensions, so they
  run an armv7 Linux build instead, keeping
  [cli-packaging](/quest/m2/cli-packaging.md)'s Pi 4 V4L2 publishing and
  [embedded video](/quest/m3/video-embedded.md) on those boards.
- moq-tokio's and moq-quic's `aws-lc-rs` and `ring` features are removed, not
  kept as alternatives, so there is one provider and no exclusion gate.
- The migration waits for rind's armv7 support, so no target is dropped.
  Decided 2026-10-31 (maintainer): each area waits only on the rind pieces it
  uses, so most areas switch once rind 0.1.0 is on crates.io (published
  2026-10-31), and only relay
  ACME waits on `rind-instant-acme`. aws-lc-rs stays in the workspace until
  the last area lands, which is when this epic removes it.
- Split per area, each landing once rind covers it. Docs a change makes stale
  (CPU requirements, removed features) are edited inline in that area's PR; no
  separate docs page.

The epic's own work, once every child has merged:

- Remove aws-lc-rs and ring from the workspace, and add a CI guard that no
  release target's graph holds aws-lc-sys, ring, or `cc` through a crypto
  crate.
- Add an `armv7-unknown-linux-gnueabihf` moq-cli release with v4l2, and point
  `doc/bin/cli.md`'s board note at it for the Pi 4, CM4, and Zero 2 W.

## Required

- [rind-instant-acme is on crates.io](/quest/m2/rind/instant-acme.md) - waiting on the adapter relay ACME needs
- [Drop iroh](/quest/m2/rind/drop-iroh.md) - moq-tokio, moq-relay, and moq-cli lose the `iroh` feature and web-transport-iroh
- [Transport on rind](/quest/m2/rind/transport.md) - moq-quic, moq-tokio, moq-uring, moq-rtmp, and qmux take rind, and the `aws-lc-rs`/`ring` features go
- [moq-auth on rind](/quest/m2/rind/auth.md) - JWT signing, key generation, and JWK loading run on rind
- [moq-e2ee on rind](/quest/m2/rind/e2ee.md) - frame and name protection use rind's AES-128-GCM, HKDF, and HMAC
- [moq-rtc on rind](/quest/m2/rind/rtc.md) - str0m and dimpl run DTLS, SRTP, and STUN on rind
- [Relay ACME on instant-acme](/quest/m2/rind/acme.md) - moq-relay replaces rustls-acme with instant-acme on rind and owns renewal
- [moq-cli and moq-hls on rind](/quest/m2/rind/cli.md) - object_store, reqwest, and axum-server take rind's provider
