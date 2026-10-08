# [L] Rust protected publisher seams

## Goal

Rust media and catalog publishers accept opaque physical names and emit no plaintext semantic catalog in E2EE mode.

The core produces only under an epoch it minted itself. The reusable crypto crate remains independent of Hang and codec-specific publisher policy.

## Plan

- Core change (decided 2026-10-08): `Credential::generation(epoch)` (`rs/moq-e2ee/src/credential.rs`) lets a caller produce under any epoch. Replace it with a minting call that takes no epoch and returns the producing generation, plus a consume-only binding for a discovered epoch, as separate types so producing under a discovered or shared epoch does not compile. The known-answer vectors need fixed epochs; keep that constructor crate-private. Propose the names in the PR; the TypeScript core mirrors them.
- Provide injectable opaque-name allocation or explicit-name constructors for mux, video, audio, timeline, and catalog publishers that currently derive semantic names.
- Derive the protected catalog name from its well-known logical role, then put media roles, codecs, quality, timelines, and custom-track mappings only inside the encrypted catalog.
- Encrypt both Hang catalog representations and encrypt or suppress the MSF catalog. Compress protected catalog bodies before encryption and retain ordinary plaintext publication unchanged outside E2EE mode.
- Cover every current Rust importer and capture path so a convenience API cannot silently recreate `.avc3`, `.opus`, timeline, or other semantic suffixes.
