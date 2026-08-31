# [L] Rust protected publisher seams

## Goal

Rust media and catalog publishers accept opaque physical names and emit no plaintext semantic catalog in E2EE mode.

The reusable crypto crate remains independent of Hang and codec-specific publisher policy.

## Plan

- Provide injectable opaque-name allocation or explicit-name constructors for mux, video, audio, timeline, and catalog publishers that currently derive semantic names.
- Derive the protected catalog name from its well-known logical role, then put media roles, codecs, quality, timelines, and custom-track mappings only inside the encrypted catalog.
- Encrypt both Hang catalog representations and encrypt or suppress the MSF catalog. Compress protected catalog bodies before encryption and retain ordinary plaintext publication unchanged outside E2EE mode.
- Cover every current Rust importer and capture path so a convenience API cannot silently recreate `.avc3`, `.opus`, timeline, or other semantic suffixes.

## Required

- [Rust E2EE core](/quest/m2/e2ee/rust.md) - provides naming derivation and protected payload primitives
