# [XS] Restore comma delimiter on TLS cert/key environment variables

## Goal

`tls::Listen::cert` and `tls::Listen::key` split comma-separated environment
variable values so NixOS `services.moq-relay.tls.certs` works with two or more
certificates.

## Plan

Decided 2026-11-21 (maintainer, /quest-plan of the exhaustive pre-cut
full-codebase audit): ✅ place this packaging regression in `m0` and gate
[the release cut](/quest/m3/release/cut.md) on it (rejected: putting it in `m1`
without gating the cut).

- `nix/modules/moq-relay.nix:368-373` joins multiple certificate and key paths
  with commas into `MOQ_TLS_CERT` and `MOQ_TLS_KEY`.
- `rs/moq-tokio/src/tls.rs:1206-1211` (`tls::Listen::cert`) and `:1218-1223`
  (`tls::Listen::key`) omit `delimiter = ','` on their `#[usage(...)]`
  attributes, so multi-cert NixOS configurations pass a single comma-joined
  string as one file path and fail at startup.
- Add `delimiter = ','` to both fields and check neighboring `Vec<PathBuf>` /
  multi-value env fields in `moq-tokio` and `moq-relay` for the same omission.

Public API: additive env parsing fix (CLI flag behavior unchanged). Wire: none.
Add a unit test in `rs/moq-tokio` parsing comma-delimited `MOQ_TLS_CERT` and
`MOQ_TLS_KEY` into two paths.
