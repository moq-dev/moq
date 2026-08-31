# [L] Cross-language encrypted proof

## Goal

Browser TypeScript and native Rust exchange encrypted audio and video in both directions over moq-lite and MoQ Transport.

Ordinary relays forward, cache, and meter the proof without receiving a content key or observing semantic track names.

## Plan

- Add automated browser-to-CLI and CLI-to-browser audio/video cases for both transport dialects using the same application credential and public client APIs.
- Assert successful late subscription and decode, plaintext absence at the relay boundary, opaque physical names, deterministic failure under ciphertext or identity tampering, and a required generation change on publisher restart.
- Exercise grouped frames on both transport dialects and datagrams on moq-lite against the shared known-answer and negative vectors. Include relocation across tracks, groups, frames, and transport domains, plus downgrade, replay-window, and sequence-exhaustion cases.
- Verify the documented browser queue and native processing bounds under 20 ms Opus and representative video. Keep the proof deterministic rather than choosing new implementation defaults or adding timing sleeps.

## Required

- [Encrypted browser components](/quest/m2/e2ee/browser.md) - supplies the browser publisher and subscriber
- [Encrypted native CLI](/quest/m2/e2ee/cli.md) - supplies the Rust publisher and subscriber
