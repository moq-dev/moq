# [M] Relay config

## Goal

moq-relay and every embedder load configuration through one call that
refuses old spellings, and relay config names do not collide with module
names or each other.

## Plan

Decided 2026-10-22 (API audit). The public implementation is checkpointed in
[moq-dev/moq#6535](https://github.com/moq-dev/moq/pull/6535):

- `Config::merge(&CliLayer)` reads the optional file and environment, reuses
  the existing Merge/Resolved provenance, and retains embedded CLI-only fields.
- Limits live under `[limits.client]` / `[limits.peer]`; ACME uses `cache` and
  a default-enabled `acme` dependency feature; dynamic peers use
  `[cluster.api] url` and `tls.root`.
- Verification concurrency is a private production constant, with a private
  test override; the benchmark sweeps offered concurrency and token payload.
- Extension fields were not unused: the relay enables IETF and forwards its
  offers to Web. Preserve the typed Rust fields and native setup API, refuse
  their TOML surface including explicit false, and keep Web setup crate-private.
- Worker binding and spawning use `Workers` → `Spawned`; only the spawned
  owner serves. Cluster API TLS comes from the existing dial template.
- mTLS policy uses one optional `Link::{Peer, Upstream}` role, preserving
  the `Grant` JSON booleans. Test-only admission helpers are private;
  in-memory `Incoming` / `Accepted` exports require `test-support`.
- Tracked demo, packaging, and test relay fixtures use the shared loader;
  the intentional published legacy fixture is checked for refusal.

Remaining completion prerequisite, not an unresolved public API policy:

1. Land the upstream implementation.
2. Advance each pinned embedding consumer's dependency through its normal
   upstream-first process.
3. Replace that consumer's direct TOML/update setup with the shared loader,
   migrate renamed settings and native types, and run its own required checks.
   Verify explicit obsolete file/environment spellings refuse startup and
   embedded CLI-only fields survive, without default-value comparisons.
4. Remove this quest once every embedder uses the one-call loader.

Do not pre-migrate consumers while their dependency pin lacks this API, add
compatibility aliases, or combine unrelated pending transport changes here.

Docs: `doc/bin/relay/`. Public API: moq-relay/moq-auth native configuration and
moq CLI breaking, with package migrations. Wire: unchanged.
