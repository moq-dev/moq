# [L] Verus proves moq-net's subscription accounting

## Goal

Verus proves `SubscribeAccounting.tla`'s accounting invariants
(`rs/moq-net/verify/subscribe/`) directly on the Rust code in
`rs/moq-net/src/lite/publisher.rs`, `lite/subscriber.rs` (with `fills.rs` and
`fit.rs`), and `src/tail.rs`. Plain builds compile no Verus crate, and a CI
job checks the proof.

## Plan

Decided 2026-11-27 (maintainer, /quest-plan on kio's follow-ups): ✅ m5,
speculative (rejected: m4; drop it). No consumer needs a proof on the code
yet, and the TLA+ model already checks the design.

- The deleted `quest/m9/verus-kio-log.md` (removed in 0e5fe75cdd) planned
  this after kio's log proof, if that proof's cost was acceptable.
- kio now proves `src/log.rs` in CI
  ([kixel-ai/kio#12](https://github.com/kixel-ai/kio/pull/12); its justfile's
  `verify` recipe and CONTRIBUTING.md's "Verified code"). Its cost is the
  reference: about 2.1 proof lines per code line, 46 trusted sites, plain
  build time unchanged, and `just verify` 23 s cold and 2.4 s warm. Copy its
  layout: the proof in attribute syntax so the files stay plain Rust, and a
  verify-only package that alone depends on `vstd`.
- Start after [moq-dev/moq#9719](https://github.com/moq-dev/moq/pull/9719)
  and [moq-dev/moq#9743](https://github.com/moq-dev/moq/pull/9743) land or
  close, since they change this code and the spec.

Public API: none. Wire: none.

## Related

- [Subscription trace validation](/quest/m4/subscribe-trace-validation.md) - checks the same model against recorded runs rather than proving it
