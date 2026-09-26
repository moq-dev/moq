# Publisher retention interop

`just test max-age` runs real WebSocket/QMux sessions through a Rust origin relay.
Rust-to-Rust, Rust-to-JavaScript, and JavaScript-to-Rust check omitted, zero, and
30-second publisher limits on lite-07, IETF draft 17, and IETF draft 22. The relay
has a one-second local cache ceiling; received metadata must remain unchanged.

The JavaScript client uses Bun and the source checkout. Every listener binds an
OS-assigned port. The interop workflow runs both tests; normal Rust CI runs the
Rust-only test. Codec regressions separately cover receive-only IETF drafts 14–16
and the published lite layouts.
