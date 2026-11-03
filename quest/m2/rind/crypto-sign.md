# [S] moq-crypto's signing runs on rind

## Goal

moq-crypto's `sign` half signs, verifies, and generates Ed25519 keys and
randomness through rind instead of aws-lc-rs, so moq-crypto depends on no
aws-lc-rs at all. Keys and signatures already written stay byte-identical.

## Plan

Decided 2026-11-03 (maintainer, /quest-plan of the follow-ups from
[moq-dev/moq#7702](https://github.com/moq-dev/moq/pull/7702)): milestone m2,
a child of the rind epic. The epic had no child for signing, and its final
step (removing aws-lc-rs) can't land until this one does.

- Today `rs/moq-sign/src/key.rs` uses `aws_lc_rs::signature::{ED25519,
  Ed25519KeyPair, …}` and `aws_lc_rs::rand`. #7702 moves it into
  `rs/moq-crypto`, whose `Cargo.toml` then holds both aws-lc-rs and rind.
- Follow [moq-dev/moq#7570](https://github.com/moq-dev/moq/pull/7570)
  (merged), whose `mesh::Signer` uses rind's `Ed25519KeyPair`. No binary
  links moq-crypto's signing yet, so under the epic's rule it can switch on
  rind 0.1.2 now.
- Test that keys and signatures from the aws-lc-rs build round-trip through
  the rind build (fixed vectors), so stored keys keep working.
- Overlap: [the mesh key codec](/quest/m1/mesh-key-codec.md)
  ([moq-dev/moq#7733](https://github.com/moq-dev/moq/pull/7733)) makes
  moq-auth use moq-sign's key type, which pulls aws-lc-rs into auth. This
  quest removes that blocker. Whichever lands second rebases onto the
  other's crate layout.

Public API: none expected. Wire: none.

## Required

- [Merge moq-sign and moq-e2ee into moq-crypto](/quest/m1/moq-crypto.md) - the crate this changes
