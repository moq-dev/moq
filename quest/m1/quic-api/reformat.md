# [M] moq-quic takes the repository's formatting

## Goal

moq-quic, moq-sock's `udp` module and tests, and moq-tokio's `quic::driver`
are formatted with the workspace rustfmt like every other crate. The quinn
`.rustfmt.toml` files, the `#[rustfmt::skip]` module guards, the separate
`rustfmt` lines in `rs/justfile`'s check and fix recipes, and the sed-based
cherry-pick recipes in `rs/moq-quic/README.md` are gone. The README keeps the
fork point, the list of upstream fixes already ported, and the carried
changes, and says future upstream fixes are ported by hand.

## Plan

Decided 2026-10-23 (maintainer): rename freely; quinn is a reference, not an
upstream we patch from, so its formatting no longer pays for itself. This
quest lands first so the reshape children diff against the final style.

A mechanical change with no semantic Rust edits: rustfmt's output, plus the
config, module-guard, `rs/justfile`, and README cleanup named in the Goal.
Review the Rust diff as formatting noise and the cleanup separately. Land it while no
other moq-quic PR is open, or rerun the formatter over the conflict.

## Required

- [Receive pinning](/quest/m1/quic-receive-pinning.md) - fix quests ranked ahead land before the whole-crate diff
- [Lease bookkeeping](/quest/m1/quic-lease-bookkeeping.md) - same

## Related

- [Lint audit](/quest/m1/audit-lints.md) - moves moq-quic onto `lints.workspace`; the same "stay close to quinn" reason goes
