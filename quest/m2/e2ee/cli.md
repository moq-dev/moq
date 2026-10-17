# [L] Encrypted native CLI

## Goal

The native CLI publishes and plays encrypted audio and video without exposing credentials through process metadata or logs.

Existing plaintext commands remain the default; an explicit application credential selects protected publication or consumption with no fallback.

## Plan

- Integrate the Rust E2EE layer into `moq-cli` publication and playback before semantic mux output and after protected track input. Keep codec, capture, playback, and synchronization logic outside the crypto layer.
- Accept the credential through a dedicated file descriptor, otherwise-unused stdin, or a permission-checked file (`0600` on Unix), never a command argument or environment variable. Redact errors and tracing, zeroize owned secret bytes, and document shell-safe invocation.
- Publish into an unannounced local broadcast as for plaintext and carry it with `moq_mux::e2ee::Producer`, which renames every catalogued track, encrypts both Hang catalogs, and drops the MSF one. Take the semantic broadcast name from the user and announce under its opaque derivation with the generation's epoch on the route; discover the newest epoch under the opaque prefix when playing, and decrypt the protected catalog and tracks before existing selection, ideally as the consuming mirror of that producer in `moq-mux`.
- Refuse `--epoch` together with a credential, and any other path that hands an encrypting publisher an epoch it did not mint, before connecting (maintainer, 2026-10-08): a shared epoch repeats AES-GCM nonces.
- Refuse `--stats` and `--echo` together with a credential: they would publish rendition IDs and per-track counters in plaintext beside the protected broadcast. This quest owns the refusal (decided 2026-10-08), since the stats quests have no encrypted mode to refuse until this credential exists.
- Cover native audio/video publication and playback, late subscription, a restarted publisher under a new epoch, `--epoch` with a credential refused, `--stats` or `--echo` with a credential refused, clean authentication errors, and both lite and IETF transports.
