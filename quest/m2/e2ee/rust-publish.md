# [L] Rust protected publisher seams

## Goal

Rust media and catalog publishers accept opaque physical names and emit no plaintext semantic catalog in E2EE mode.

The reusable crypto crate remains independent of Hang and codec-specific publisher policy.

## Plan

Publishers use `Credential::produce()` and its `generation::Producer`; the
producing/consuming split landed separately in #5313.

- Adapt mux, video, audio, timeline, and catalog publishers to the e2ee
  broadcast wrappers (`create_track`, `reserve_track`). Every track these
  publishers serve is declared in advance, so this does not wait on the
  [dynamic names](/quest/m2/e2ee/dynamic-names.md) design (maintainer,
  2026-10-19). Applications keep semantic track names; the wrappers own
  their translation to physical names, as the epic specifies.
- Derive the protected catalog name from its well-known logical role, then put media roles, codecs, quality, timelines, and custom-track mappings only inside the encrypted catalog.
- Encrypt both Hang catalog representations and encrypt or suppress the MSF catalog. Compress protected catalog bodies before encryption and retain ordinary plaintext publication unchanged outside E2EE mode.
- Cover every current Rust importer and capture path so a convenience API cannot silently recreate `.avc3`, `.opus`, timeline, or other semantic suffixes.
