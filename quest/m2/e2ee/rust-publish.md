# [M] Rust protected publisher seams

## Goal

Rust media and catalog publishers emit no plaintext semantic catalog in E2EE
mode.

The reusable crypto crate remains independent of Hang and codec-specific
publisher policy.

## Plan

Publishers use `Credential::produce()` and its `generation::Producer`; the
producing/consuming split landed separately in #5313.

Decided 2026-10-20 (maintainer): the e2ee wrappers implement the
[moq-net traits](/quest/m2/net-wrap/README.md), and moq-mux and the
moq-video and moq-audio encoders take them, so mux, video, audio, and
timeline publishers already publish into an encrypted
broadcast under semantic names with no change here. This quest keeps only
what the traits do not cover. Every track these publishers serve is declared
in advance, so it does not wait on
[undeclared names](/quest/m2/e2ee/undeclared-names.md) (maintainer,
2026-10-19).

- Catalog naming: derive the protected catalog name from its well-known
  logical role, then put media roles, codecs, quality, timelines, and
  custom-track mappings only inside the encrypted catalog.
- Encrypt the single Hang `catalog.json` left by the
  [m1 compression migration](/quest/m1/compress/README.md), and encrypt or
  suppress the MSF catalog. Set logical `compress` on protected catalog
  tracks; the [e2ee wrapper](/quest/m2/net-wrap/e2ee.md) compresses before
  encryption. Retain ordinary plaintext publication unchanged outside E2EE mode.
- Cover every current Rust importer and capture path so a convenience API
  cannot emit a plaintext semantic catalog.
- A re-created track resumes (decided 2026-10-25, maintainer, replacing the
  2026-10-17 refusal): moq-e2ee never releases a name's claim, and a track
  created again under a claimed name continues above its high-water marks,
  so nonces never repeat. [Undeclared names](/quest/m2/e2ee/undeclared-names.md)
  owns that `create_track` change and its test, so this quest still does not
  wait on it.

Public API: none. Wire: none.

## Required

- [moq-e2ee implements the traits](/quest/m2/net-wrap/e2ee.md) - the encrypted broadcast moq-mux publishes into
- [moq-mux takes the traits](/quest/m2/net-wrap/mux.md) - the mux and capture publishers this relies on to stay unchanged
