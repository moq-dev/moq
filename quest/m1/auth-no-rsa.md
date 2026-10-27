# [S] moq-auth without the rsa crate

## Goal

moq-auth generates and loads RSA keys through aws-lc-rs and the `pkcs1` crate,
so no production crate's graph holds the `rsa` crate (`cargo tree -e normal,build
-i rsa` on moq-relay, moq-cli, and moq-room prints nothing). Every algorithm
moq-auth accepts today still signs and verifies.

## Plan

Source is implemented in [PR #6898](https://github.com/moq-dev/moq/pull/6898).
Keep it draft until the required local gates pass. Remaining:

- Fix the unrelated main failures separately: CARLA rustfmt differences under
  `demo/carla/bridge`, and `rs/moq-mux/benches/catalog.rs:226` referencing removed
  `hang::catalog::MAX_RENDITIONS`. No open fix for either was found at checkpoint.
  Incidental CARLA formatting from the auth-scoped fix was restored.
- Resolve the local flake check's runner-image build restriction:
  `systemd-repart` cannot write `/proc/self/uid_map` (`Operation not permitted`).
  Evaluation alone does not count as a passing flake check.
- Resume the prescribed `just fix` and clean committed-head `just ci` after
  those prerequisites clear, then finish ready/review. Preserve the auth scope.

The maintainer's 2026-10-26 decisions remain settled: generate 2048-bit RSA with
aws-lc-rs, read its PKCS#8 components with `pkcs8`/`pkcs1`, and encode all eight
supplied components directly as PKCS#1 DER for jsonwebtoken. Require complete
private CRT parameters, refuse multi-prime `oth`, and remove only the two
`From<rsa::Error>` / `From<rsa::pkcs1::Error>` conversions. Wire and key formats
stay unchanged. The [graviola switch](/quest/m3/pure-crypto/README.md) is deferred.
No maintainer policy decision remains.

`deny.toml` retains the advisory exception only for published `moq-auth` 0.2.1
and `moq-token` 0.7 compatibility dev fixtures. Remove it when those fixtures
move to releases without `rsa`.

### Proof at checkpoint

Before fixing, both incomplete-private and `oth` tests failed. Afterward all
229 focused Rust auth tests and 82 JavaScript key/interop tests passed, including
corrupted CRT values. Independent selected phases on the source commit passed
10,501 Rust tests (111 configured skips), JavaScript checks/builds, and 4,976
JavaScript tests. Markdown, TOML, quest validation, and GitHub tooling tests pass.

The actual CLI generated, saved, signed, and verified all 12 algorithms with
owner-only private files, rejected each missing CRT field, and passed
bidirectional Rust/JavaScript interoperability for all six RSA algorithms.
Relay, CLI, and room normal/build trees have no `rsa`; Cargo reports no matching
package for inverse queries and forward trees independently confirm absence.
Matching JavaScript imports refuse `oth` instead of dropping it, with both
migration guides documenting the behavior and removed Rust conversions.

Auth-scoped `just rs fix` passes. Full `just fix` fails at the unrelated benchmark
reference above. Clean-head `just ci` fails in inherited CARLA rustfmt, cancelling
other lanes; the independent passing phases do not constitute a passing full
gate. Separate `nix flake check` fails on the host restriction above. Diagnostics
are retained in the isolated checkout's `.tmp/`; do not rerun unchanged failures
to green or fold their fixes into this PR.
