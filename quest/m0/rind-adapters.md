# [S] MoQ builds against rind's corrected adapters

## Goal

`main` builds and passes its checks against the patch releases of rind-rustls,
rind-jsonwebtoken, rind-object-store, and rind-rcgen that make each adapter's
API its upstream's, on the day they publish: the PR is ready before they
publish and merges when they do.

Non-goals: rind 0.4, which removes `rind::cpu::check()` and is a later,
separate move; and the dimpl and str0m providers vendored under
`rs/moq-webrtc/src/vendor`, which are MoQ's own code and depend on neither
rind-dimpl nor rind-str0m.

## Plan

Decided 2026-11-09 (maintainer, rind's quest audit): one rind 0.4 that carries
all three breaking changes, and a MoQ-side PR ready before the m0 adapter patch
releases publish ✅ (rejected: the 0.4 quest alone, accepting that MoQ's build
breaks until fixed; leaving both to release-plz). rind holds those releases
until this PR is ready
([its epic](https://github.com/moq-dev/rind/blob/main/quest/m0/adapters/README.md)).

rind treats an adapter API that differs from its upstream's as a bug and fixes
it in a patch release, so the fixes arrive inside MoQ's caret requirements
(`Cargo.toml:218-221`). `Cargo.lock` keeps `main` building until it moves
(Dependabot, `.github/dependabot.yml`, or a hand update); the first update
after the releases fails to compile. No crate published from MoQ names a rind
adapter yet (crates.io's moq-auth 0.2.4 and moq-token 0.7.4 still require
aws-lc-rs), so `main` and the next cut are exposed, not a shipped release.

Lines below are at `ecfa75a0` (2026-11-09). Three rind PRs change what MoQ
calls:

- [moq-dev/rind#521](https://github.com/moq-dev/rind/pull/521), rind-rustls:
  `default_provider()` returns `CryptoProvider`, no longer a `Result`. Drop
  the `.expect(..)`, `.unwrap()`, or `?` after it.
- [moq-dev/rind#519](https://github.com/moq-dev/rind/pull/519),
  rind-jsonwebtoken: `default_provider()` becomes the static
  `DEFAULT_PROVIDER`, so `rind_jsonwebtoken::default_provider().expect(..)`
  becomes `&rind_jsonwebtoken::DEFAULT_PROVIDER`. rind-object-store:
  `default_provider()` returns `Arc<dyn CryptoProvider>`, no longer a
  `Result`; drop the `?`.
- [moq-dev/rind#515](https://github.com/moq-dev/rind/pull/515), rind-rcgen:
  `KeyPair::generate(&alg)` becomes `KeyPair::generate_for(&alg)`
  (`generate()` now takes no argument and makes a P-256 key), and
  `to_pkcs8()` is gone. `key.to_pkcs8().secret_pkcs8_der().to_vec()` becomes
  `key.serialize_der()`, and a borrowed `key.to_pkcs8().secret_pkcs8_der()`
  becomes `key.serialized_der()`.

Shipped code, each call and what it becomes:

- `rs/moq-crypto/src/provider.rs:24` (rind-rustls):
  `rind_rustls::default_provider().expect(..)` becomes
  `rind_rustls::default_provider()`. `crate::cpu::check()?` on line 22 stays
  until rind 0.4.
- `rs/moq-auth/src/crypto.rs:7` (rind-jsonwebtoken):
  `rind_jsonwebtoken::default_provider().expect(..)` becomes
  `&rind_jsonwebtoken::DEFAULT_PROVIDER`; its test at line 37 likewise.
- `rs/moq-cli/src/archive.rs:330`, `:338`, `:346` (rind-object-store):
  `rind_object_store::default_provider()?` becomes
  `rind_object_store::default_provider()`.
- `rs/moq-tokio/src/tls.rs:1260` (rind-rcgen):
  `rind_rcgen::KeyPair::generate(..)` becomes `generate_for(..)`; line 1272's
  `PrivatePkcs8KeyDer::from(key_pair.to_pkcs8().secret_pkcs8_der().to_vec())`
  becomes `PrivatePkcs8KeyDer::from(key_pair.serialize_der())`. The serial it
  sets on lines 1263-1265 already covers rind-rcgen's removed
  `serial_number()`.
- `rs/moq-webrtc/src/vendor/dimpl/certificate.rs:39` (rind-rcgen):
  `generate(..)` becomes `generate_for(..)`; line 76's
  `key_pair.to_pkcs8().secret_pkcs8_der().to_vec()` becomes
  `key_pair.serialize_der()`.
- `rs/moq-relay/examples/embed.rs:23` (rind-rustls): drop the `.unwrap()`
  after `rind_rustls::default_provider()`.

Tests, benches, and test modules make the same three calls many more times,
all the same mechanical edits. Find them with:

- `rind_rustls::default_provider()`: 105 lines in 40 files, plus 5 bare
  `default_provider()` calls in `rs/moq-quic/src/tests/mod.rs` and
  `rs/moq-tokio/src/quic/driver/tests.rs`, which import it by `use`.
- `KeyPair::generate(&`: 78 lines in 38 files. One asks for P-384,
  `rs/moq-webrtc/src/vendor/dimpl/tests/dtls12/handshake.rs:841`, so use
  `generate_for` everywhere rather than the P-256 default.
- `to_pkcs8().secret_pkcs8_der()`: 60 lines in 33 files. The other
  `to_pkcs8()` calls (`rs/moq-auth/src/key.rs:574`, `:587`, and
  `rs/moq-auth/src/generate.rs:41`) are rind's own key types and do not
  change.

MoQ calls none of the other items those PRs remove (rind-rustls's
`MissingFeature`, `empty_provider()`, `verify`, `sign` statics, and
`kx_group::SECP521R1`; rind-rcgen's `check()`, `serial_number()`, and
`key_identifier()`), by grep at that commit.

Raise the four requirements in `Cargo.toml:218-221` to the releases that carry
the fixes, since the new calls do not compile against the old ones.

Until those releases exist, check the branch with an uncommitted
`[patch.crates-io]` that points `rind` and the four adapters at moq-dev/rind's
`main` once the three PRs merge. The rind-rcgen PR and the one for
rind-jsonwebtoken and rind-object-store are still drafts, so re-read each
PR's Public API section when it merges; a signature may have moved.

Public API: none; `moq_crypto::provider` keeps its signatures. Wire: none.

## Waiting

- moq-dev/rind: publishes the adapter patch releases that carry the three PRs
  above, which it does once this PR is ready. Check that the three PRs are
  merged and that `cargo search rind-rustls`, `rind-jsonwebtoken`,
  `rind-object-store`, and `rind-rcgen` each show a release newer than its
  merge; then set the requirements, update `Cargo.lock`, and merge.
