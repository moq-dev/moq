# [L] Encrypted native CLI

## Goal

The native CLI publishes and plays encrypted audio and video without exposing credentials through process metadata or logs.

Existing plaintext commands remain the default; an explicit application credential selects protected publication or consumption with no fallback.

## Plan

- Integrate the Rust E2EE layer into `moq-cli` publication and playback before semantic mux output and after protected track input. Keep codec, capture, playback, and synchronization logic outside the crypto layer.
- Accept the credential through a dedicated file descriptor, otherwise-unused stdin, or a permission-checked file (`0600` on Unix), never a command argument or environment variable. Redact errors and tracing, zeroize owned secret bytes, and document shell-safe invocation.
- Inject opaque physical names into mux, video, audio, timeline, and catalog construction. Take the semantic broadcast name from the user and publish under its opaque derivation; mint a UUIDv7 epoch per publisher run and discover the newest one under the opaque prefix when playing; derive and decrypt the protected catalog before existing selection, and suppress every plaintext Hang or MSF catalog representation.
- Refuse any path that hands an encrypting publisher an epoch it did not mint, before connecting (maintainer, 2026-10-08): a shared epoch repeats AES-GCM nonces. `moq --epoch` is removed by [distinct importer epochs](/quest/m0/broadcast-epoch/distinct-importer-epochs.md).
- Refuse `--stats` and `--echo` together with a credential: they would publish rendition IDs and per-track counters in plaintext beside the protected broadcast. This quest owns the refusal (decided 2026-10-08), since the stats quests have no encrypted mode to refuse until this credential exists.
- Cover native audio/video publication and playback, late subscription, a restarted publisher under a new epoch, `--stats` or `--echo` with a credential refused, clean authentication errors, and both lite and IETF transports.

## Required

- [Rust protected publisher seams](/quest/m2/e2ee/rust-publish.md) - prevents native convenience publishers from leaking semantic names or catalogs
- [Encrypted consumer shape](/quest/m2/e2ee/consumer.md) - playback uses the shared consumer instead of its own decryption
