# [M] Rust protected publisher seams

## Goal

Rust media and catalog publishers emit no plaintext semantic catalog in E2EE
mode, and a track name the e2ee producer already carried is refused loudly
rather than silently left out.

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
  [m1 compression migration](/quest/m1/compress/migrate.md), and encrypt or
  suppress the MSF catalog. Set logical `compress` on protected catalog
  tracks; the [e2ee wrapper](/quest/m2/net-wrap/e2ee.md) compresses before
  encryption. Retain ordinary plaintext publication unchanged outside E2EE mode.
- Cover every current Rust importer and capture path so a convenience API
  cannot emit a plaintext semantic catalog.
- Refuse a re-created track (decided 2026-10-17, maintainer): moq-e2ee
  never releases a name's claim, because reopening would restart its nonce
  counters, so a track created again under a name the producer already
  carried fails with a new error instead of being dropped. A track name
  always means the same content, so a restarted encoder publishes a new name
  or a new broadcast. Test it through moq-mux.

Public API: a new error from the e2ee producer. Wire: none.

## Required

- [moq-e2ee implements the traits](/quest/m2/net-wrap/e2ee.md) - the encrypted broadcast moq-mux publishes into
- [moq-mux takes the traits](/quest/m2/net-wrap/mux.md) - the mux and capture publishers this relies on to stay unchanged
