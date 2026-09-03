# [L] Encrypted native CLI

## Goal

The native CLI publishes and plays encrypted audio and video without exposing credentials through process metadata or logs.

Existing plaintext commands remain the default; an explicit application credential selects `.e2ee` publication or consumption with no fallback.

## Plan

- Integrate the Rust E2EE layer into `moq-cli` publication and playback before semantic mux output and after protected track input. Keep codec, capture, playback, and synchronization logic outside the crypto layer.
- Accept the credential through a dedicated file descriptor, otherwise-unused stdin, or a permission-checked file (`0600` on Unix), never a command argument or environment variable. Redact errors and tracing, zeroize owned secret bytes, and document shell-safe invocation.
- Inject opaque physical names into mux, video, audio, timeline, and catalog construction. Derive and decrypt the protected catalog before existing selection, and suppress every plaintext Hang or MSF catalog representation.
- Cover native audio/video publication and playback, late subscription, generation replacement, retransmission, clean authentication errors, and both lite and IETF transports.

## Required

- [Rust E2EE core](/quest/m2/e2ee/rust.md) - provides native encryption, decryption, naming, and failure behavior
- [Rust protected publisher seams](/quest/m2/e2ee/rust-publish.md) - prevents native convenience publishers from leaking semantic names or catalogs
