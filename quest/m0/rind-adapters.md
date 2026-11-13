# [S] MoQ builds against rind 0.4 and its corrected adapters

## Goal

`main` builds and passes its checks against rind 0.4 and the patch releases of
rind-rustls, rind-jsonwebtoken, rind-object-store, and rind-rcgen that make
each adapter's API its upstream's, on the day they publish: the PR is ready
before they publish and merges when they do.

Non-goal: replacing the dimpl and str0m providers vendored under
`rs/moq-webrtc/src/vendor`, which are MoQ's own code and depend on neither
rind-dimpl nor rind-str0m. They call rind directly, so rind 0.4 still reaches
them.

## Plan

Decided 2026-11-09 (maintainer, rind's quest audit): one rind 0.4 that carries
all three breaking changes, and a MoQ-side PR ready before the m0 adapter patch
releases publish ✅ (rejected: the 0.4 quest alone, accepting that MoQ's build
breaks until fixed; leaving both to release-plz). rind holds those releases
until this PR is ready.

The same day the plan changed: two of rind 0.4's breaking changes merged while
rind's 0.3.3 was unpublished, so the maintainer chose ✅ to set rind to 0.4.0
and hold the release until the last one, the CPU-check removal, lands
(rejected: ship 0.4.0 now and take the CPU-check removal in 0.5; revert both
and release 0.3.3). So the adapter fixes and rind 0.4 publish in one release
([kixel-ai/rind#548](https://github.com/kixel-ai/rind/pull/548)), each adapter
requires rind 0.4, and this PR moves MoQ to both at once. Moving to the
adapters alone would leave two copies of rind in the graph.

rind treats an adapter API that differs from its upstream's as a bug and fixes
it in a patch release, so the fixes arrive inside MoQ's caret requirements
(`Cargo.toml:218-221`). `Cargo.lock` keeps `main` building until it moves
(Dependabot, `.github/dependabot.yml`, or a hand update); the first update
after the releases fails to compile. No crate published from MoQ names a rind
adapter yet (crates.io's moq-auth 0.2.4 and moq-token 0.7.4 still require
aws-lc-rs), so `main` and the next cut are exposed, not a shipped release.

Lines below are at `ecfa75a0` (2026-11-09). Three rind PRs change what MoQ
calls:

- [kixel-ai/rind#521](https://github.com/kixel-ai/rind/pull/521), rind-rustls:
  `default_provider()` returns `CryptoProvider`, no longer a `Result`. Drop
  the `.expect(..)`, `.unwrap()`, or `?` after it.
- [kixel-ai/rind#519](https://github.com/kixel-ai/rind/pull/519),
  rind-jsonwebtoken: `default_provider()` becomes the static
  `DEFAULT_PROVIDER`, so `rind_jsonwebtoken::default_provider().expect(..)`
  becomes `&rind_jsonwebtoken::DEFAULT_PROVIDER`. rind-object-store:
  `default_provider()` returns `Arc<dyn CryptoProvider>`, no longer a
  `Result`; drop the `?`.
- [kixel-ai/rind#515](https://github.com/kixel-ai/rind/pull/515), rind-rcgen:
  `KeyPair::generate(&alg)` becomes `KeyPair::generate_for(&alg)`
  (`generate()` now takes no argument and makes a P-256 key), and
  `to_pkcs8()` is gone. `key.to_pkcs8().secret_pkcs8_der().to_vec()` becomes
  `key.serialize_der()`, and a borrowed `key.to_pkcs8().secret_pkcs8_der()`
  becomes `key.serialized_der()`.

Shipped code, each call and what it becomes:

- `rs/moq-crypto/src/provider.rs:24` (rind-rustls):
  `rind_rustls::default_provider().expect(..)` becomes
  `rind_rustls::default_provider()`. `crate::cpu::check()?` on line 22 goes
  with the CPU check below.
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

rind 0.4 itself changes three things, of which MoQ uses two:

- No runtime CPU check: `rind::cpu::check` and `rind::cpu::MissingFeature` are
  gone, since a backend now requires only its target's compile-time features.
  `rs/moq-crypto/src/cpu.rs` wraps both (`UnsupportedCpu` at line 6, `check()`
  at line 17). Recommended: delete `moq_crypto::cpu::check` and
  `UnsupportedCpu`, keeping the `Dit` re-export, rather than leave a check that
  cannot fail. That removes public items from moq-crypto, so say so in the PR.
  `rg 'cpu::check|MissingFeature|UnsupportedCpu' rs` finds every site; at
  `985e8897` they are:
  - `moq_crypto::cpu::check()` callers: `rs/moq-crypto/src/provider.rs:22`
    and `:48`, `rs/moq-crypto/src/e2ee/credential.rs:60`,
    `rs/moq-auth/src/crypto.rs:6`, `rs/moq-hls/src/server/mod.rs:188`,
    `rs/moq-hls/src/export/mod.rs:147`,
    `rs/moq-quic/src/config/endpoint.rs:122`, `rs/moq-rtmp/src/dial.rs:84`
    and `:104`, `rs/moq-rtmp/src/server.rs:269` and `:499`,
    `rs/moq-uring/src/quic/driver/mod.rs:126`,
    `rs/moq-webrtc/src/session.rs:674`, and `rs/qmux/src/tls.rs:76`.
  - The `UnsupportedCpu` error variants and re-export that wrap it, in
    moq-auth, moq-crypto's e2ee, moq-hls, moq-quic, moq-rtmp, moq-tokio
    (`rs/moq-tokio/src/crypto.rs:7`), moq-uring, moq-webrtc, and qmux, with
    the doc comments that name it.
  - The vendored providers, which call rind directly:
    `rs/moq-webrtc/src/vendor/dimpl/crypto/rind/mod.rs:24-25` and
    `rs/moq-webrtc/src/vendor/str0m_rind/lib.rs:19-20` return
    `rind::cpu::MissingFeature` after `rind::cpu::check()`, so their
    `default_provider()` becomes infallible, and
    `rs/moq-webrtc/src/vendor/dimpl/error.rs:363` drops
    `UnsupportedCpu(rind::cpu::MissingFeature)`.
- Agreement keys keep their public key
  ([kixel-ai/rind#529](https://github.com/kixel-ai/rind/pull/529)):
  `agreement::PrivateKey::public_key()` returns a borrow. The vendored dimpl
  provider calls it at `rs/moq-webrtc/src/vendor/dimpl/crypto/rind/kx.rs:38`;
  check that line and the rest of that file against #529's Public API section.
- AES-CBC encryption takes a typed `CbcIv`
  ([kixel-ai/rind#530](https://github.com/kixel-ai/rind/pull/530)): MoQ calls no
  AES-CBC encryption, by grep at that commit.

Raise `rind` to 0.4 (`Cargo.toml:217`, keeping `wipe`) and the four adapter
requirements in `Cargo.toml:218-221` to the releases that carry the fixes,
since the new calls do not compile against the old ones.

Until those releases exist, check the branch with an uncommitted
`[patch.crates-io]` that points `rind` and the four adapters at kixel-ai/rind's
`main` once the three PRs merge. #521 and #515 have merged; #519 is open and
ready for review, so re-read its Public API section when it merges; a
signature may have moved.

Public API: `moq_crypto::provider` keeps its signatures; `moq_crypto::cpu::check` and `UnsupportedCpu` go if the recommendation above is taken. Wire: none.

## Waiting

- kixel-ai/rind: publishes rind 0.4.0 and the adapter patch releases in one
  release, once its CPU-check removal has landed and this PR is ready. Check
  that `cargo search rind` shows 0.4.0, and that each of rind-rustls,
  rind-jsonwebtoken, rind-object-store, and rind-rcgen has a release whose
  `rind` requirement is 0.4, read from crates.io's
  `https://crates.io/api/v1/crates/<crate>/<version>/dependencies`; then set
  the requirements, update `Cargo.lock`, and merge.
