# [M] moq-tokio: compile with any subset of transport features

## Goal

`cargo check -p moq-tokio --no-default-features` and every single-feature
build compile. A crate with no transport at all builds into something that
cannot connect; a `tcp` / `uds`-only build serves qmux. `moq-relay`,
`moq-cli`, `moq-bench`, and `moq-boy` compile with their own
`--no-default-features`. Nightly proves all of it per crate, so workspace
feature unification can never hide a break again.

## Plan

Decided: make it compile rather than `compile_error!`. `dev` already gates
`server`, `worker`, and `util` on a transport, hands `QuicBackend` an empty
choice list without a QUIC feature, and treats tcp-only as a real
configuration, so refusing the backend-less build would contradict the gates
the crate ships. The existing `compile_error!` for a rustls backend without a
crypto provider stays.

What breaks today, measured on the pre-rename crate:

- Zero features: four errors, all from `RequestKind` in `src/server.rs`.
  Every variant is feature-gated, so the `request_ref!` / `request_into!` /
  `request_map!` matches become non-exhaustive over an empty, still-inhabited
  enum. Fix the macros to expand to an uninhabited match (`match *self {}`)
  when no variant exists, or give the enum a gated never-variant.
- `tcp`-only relay and cli: `tls::Server::server_config` is gated on a QUIC
  backend but called unconditionally. Split the gate so only the QUIC-specific
  parts of `tls::Server` need a backend.
- The `steer` module the issue names no longer exists; `worker` and `util`
  already share one gate on `dev`.

Why nightly is green: `just rs features` runs `--workspace
--no-default-features`, and `moq-rtmp`'s dev-dependency enables
`moq-tokio/quinn` (so its RTMPS test can borrow the self-signed cert helper,
which needs `tls::Server`). Unification hands every crate a backend. Once
`tls` compiles without one, drop that pin, keeping `aws-lc-rs`.

Even then relay and cli unify `tcp` and `aws-lc-rs` into `moq-tokio`, so add
per-crate coverage to `just rs features`: `cargo hack check -p moq-tokio
--each-feature --no-dev-deps --locked`, plus `cargo check -p <crate>
--no-default-features` for `moq-relay`, `moq-cli`, `moq-bench`, and
`moq-boy`. Add `cargo-hack` to the Nix dev shell next to `cargo-deny`; if it
must be installed in CI, pin the version, since `--locked` alone does not.

Branch from `dev`: the crate exists only there.

## Closes

- [#2979](https://github.com/moq-dev/moq/issues/2979) - close this issue when the quest finishes
