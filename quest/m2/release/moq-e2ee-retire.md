# [XS] Retire the moq-e2ee stub

## Goal

`rs/moq-e2ee` is gone and the `moq-e2ee` crate is no longer published. Its
last release, a code-free crate whose README points at moq-crypto, stays on
crates.io for anyone still resolving the old name.

## Waiting

- moq-dev/moq: the next cut publishes the `moq-e2ee` stub to crates.io,
  above 0.0.20. Check with
  `curl -s https://crates.io/api/v1/crates/moq-e2ee | jq -r .crate.max_version`.

## Plan

Decided 2026-11-03 (maintainer, follow-up planning after the 2026-11-03 /quest-afk and /quest-complete runs), milestone:

- m0
- m1
- ✅ m2, in the release epic beside the libmoq stub

[moq-dev/moq#7702](https://github.com/moq-dev/moq/pull/7702) (merged) turned
`rs/moq-e2ee` into a code-free stub that the cut's `just version` lifts
above 0.0.20, so release-plz publishes it.

- Once that release is on crates.io, delete `rs/moq-e2ee` and its workspace
  member, so release-plz never publishes it again.
- Grep for any remaining `moq-e2ee` that names the crate rather than the
  draft or profile (`draft-lcurley-moq-e2ee`, "moq-e2ee epoch").

Public API: none. Wire: none.

## Related

- [Retire libmoq](/quest/m2/release/libmoq-retire.md) - the same retirement for libmoq
