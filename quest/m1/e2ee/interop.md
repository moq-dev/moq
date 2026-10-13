# [L] Cross-language encrypted proof

## Goal

Browser TypeScript and native Rust exchange encrypted audio and video in both directions over moq-lite and MoQ Transport.

Ordinary relays forward, cache, and meter the proof without receiving a content key or observing semantic track names.

## Plan

- Add automated browser-to-CLI and CLI-to-browser audio/video cases for both transport dialects using the same application credential and public client APIs.
- Assert successful late subscription and decode, plaintext absence at the relay boundary, opaque physical names, deterministic failure under ciphertext or identity tampering, and that a restarted publisher lands under a new epoch that the old keys cannot open.
- Exercise grouped frames and datagrams on both transport dialects against the shared known-answer and negative vectors, and exchange datagrams browser-to-CLI and CLI-to-browser, not only vectors. JavaScript carries MoQ Transport datagrams since [#4979](https://github.com/moq-dev/moq/pull/4979). Include relocation across tracks, groups, frames, epochs, and transport domains, plus replay-window and sequence-exhaustion cases.
- Verify the documented browser queue and native processing bounds under 20 ms Opus and representative video. Keep the proof deterministic rather than choosing new implementation defaults or adding timing sleeps.

## Required

- [Encrypted browser components](/quest/m1/e2ee/browser.md) - supplies the browser publisher and subscriber
- [Encrypted native CLI](/quest/m1/e2ee/cli.md) - supplies the Rust publisher and subscriber
