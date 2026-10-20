# [L] Encrypted browser components

## Goal

Browser publish and watch components expose encrypted audio and video without leaking credentials into browser-owned surfaces.

Existing plaintext operation remains the default; an explicit application credential selects protected publication or consumption with no fallback.

## Plan

- Integrate the TypeScript E2EE layer before the existing Hang publish and after the existing Hang subscribe boundaries, keeping codec selection, synchronization, and rendering in the post-decryption pipelines.
- Accept keys through a programmatic property/API only, never an HTML attribute, URL, persistent browser storage, analytics event, or log. Clear owned secret bytes when the component is replaced or disconnected; do not make extractability a hidden requirement.
- Derive the invariant opaque broadcast path from the semantic name and
  resolve that path, taking the selected epoch from its route. On a wire
  without route epochs, the application supplies the epoch. Derive the
  encrypted catalog name from that generation, decrypt it before existing
  Hang selection, then subscribe to its opaque media and timeline tracks.
  Publish every catalog representation and semantic track name through the
  same protected naming contract.
- Refuse the stats track and the `echo` attribute while a credential is set: they would publish rendition IDs and per-track counters in plaintext beside the protected broadcast. This quest owns the refusal (decided 2026-10-08), since the stats quests have no encrypted mode to refuse until this credential exists.
- Cover camera/microphone publication, the stats and `echo` refusal, audio/video playback, late subscription, WebCrypto backpressure through awaited writes, a restarted publisher under a new epoch, bad-group termination, and bad-datagram events in browser tests.

## Required

- [Encrypted consumer shape](/quest/m2/e2ee/consumer.md) - watch uses the shared consumer instead of its own decryption
