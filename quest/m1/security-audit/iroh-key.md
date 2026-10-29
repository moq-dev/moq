# [XS] Iroh secret written private

## Goal

A generated iroh secret key file is created with mode 0600, never through an
existing file or symlink, and a pre-existing key that is not a regular file
or is readable by group or others is refused.

## Plan

From the 2026-10-27 audit (RELAY-3). `rs/moq-tokio/src/iroh.rs` checks
`exists()` and then calls `tokio::fs::write`, which uses the umask (usually
0644), follows symlinks, and races. Reuse the TLS keylog writer's
`create_new` plus `mode(0o600)` pattern in `rs/moq-tokio/src/tls.rs`. The
`iroh` feature is off by default in moq-relay. The 2026-10-28 audit repeated
the finding and added the regular-file check on an existing key.

Public API: none. Wire: none.
