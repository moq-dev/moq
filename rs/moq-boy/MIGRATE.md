# Migrating moq-boy

How to update your invocation and config for each breaking change, newest first.

## Unreleased

### Connection retry flags have the `connect-` prefix

Rename `--backoff-*` to `--connect-backoff-*` and `--goaway-*` to `--connect-goaway-*`.
Rename `MOQ_BACKOFF_*` and `MOQ_GOAWAY_*` to `MOQ_CONNECT_BACKOFF_*` and `MOQ_CONNECT_GOAWAY_*`.
Old spellings stop startup and name their replacements.

### Key logging is opt-in (#5907)

`SSLKEYLOGFILE` alone no longer writes TLS session secrets.
Add `--quic-keylog` (`MOQ_QUIC_KEYLOG`) to capture them; without `SSLKEYLOGFILE` it stops startup.

### Build with `--features openh264` (#5771)

moq-boy encodes with OpenH264, which is now opt-in, and refuses to start without it.
Install with `cargo install moq-boy --features openh264`. The Nix package already enables it.
