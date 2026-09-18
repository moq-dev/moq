# [L] Encrypted native CLI

## Goal

The native CLI publishes and plays encrypted audio and video without exposing credentials through process metadata or logs.

Existing plaintext commands remain the default; an explicit application credential selects protected publication or consumption with no fallback.

## Plan

- Integrate the Rust E2EE layer into `moq-cli` publication and playback before semantic mux output and after protected track input. Keep codec, capture, playback, and synchronization logic outside the crypto layer.
- Accept the credential through a dedicated file descriptor, otherwise-unused stdin, or a permission-checked file (`0600` on Unix), never a command argument or environment variable. Redact errors and tracing, zeroize owned secret bytes, and document shell-safe invocation.
- Inject opaque physical names into mux, video, audio, timeline, and catalog construction. Take the semantic broadcast name from the user and publish under its opaque derivation; mint a UUIDv7 epoch per publisher run and discover the newest one under the opaque prefix when playing; derive and decrypt the protected catalog before existing selection, and suppress every plaintext Hang or MSF catalog representation.
- Cover native audio/video publication and playback, late subscription, a restarted publisher under a new epoch, clean authentication errors, and both lite and IETF transports.

## Required

- [Rust protected publisher seams](/quest/m2/e2ee/rust-publish.md) - prevents native convenience publishers from leaking semantic names or catalogs
