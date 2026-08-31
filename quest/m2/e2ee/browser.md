# [L] Encrypted browser components

## Goal

Browser publish and watch components expose encrypted audio and video without leaking credentials into browser-owned surfaces.

Existing plaintext operation remains the default; an explicit application credential selects `.e2ee` publication or consumption with no fallback.

## Plan

- Integrate the TypeScript E2EE layer before the existing Hang publish and after the existing Hang subscribe boundaries, keeping codec selection, synchronization, and rendering in the post-decryption pipelines.
- Accept keys through a programmatic property/API only, never an HTML attribute, URL, persistent browser storage, analytics event, or log. Clear owned secret bytes when the component is replaced or disconnected; do not make extractability a hidden requirement.
- Derive the encrypted catalog name from the credential, decrypt it before existing Hang selection, then subscribe to its opaque media and timeline tracks. Publish every catalog representation and semantic track name through the same protected naming contract.
- Cover camera/microphone publication, audio/video playback, late subscription, bounded WebCrypto backpressure, generation replacement, bad-group termination, and bad-datagram events in browser tests.

## Required

- [TypeScript E2EE core](/quest/m2/e2ee/typescript.md) - provides browser encryption, decryption, naming, and failure behavior
