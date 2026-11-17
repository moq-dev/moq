# [S] Relay cluster config fails loud, keeps secrets out of logs, and its upgrade notes are right

## Goal

The relay refuses cluster settings it would ignore, never echoes a secret in
an error, re-checks a refreshed grant like every other credential, resets
its redial backoff after a stable session, and its upgrade notes name real
flags.

## Plan

Found by the 2026-11-17 pre-cut correctness audit; each item confirmed from
source by its reviewer, reproduce first.

- `rs/moq-relay/src/cluster.rs`: `release` accepted the 64-hex LAN secret
  inline; `main` reads it as a file path and fails with "failed to read
  secret file <secret>", printing the mesh key into logs on upgrade. Refuse
  an inline-looking value without echoing it.
- `rs/moq-relay/src/auth/refresh.rs`: the grant re-read at protocol setup
  skips the root, mounts, link, and role checks the other credential paths
  run (`valid()`); run them.
- `rs/moq-relay/src/cluster.rs`: `run_remote_once` only returns an error, so
  the "reset after a stable session" backoff branch is unreachable and long
  outages leave every later redial at 30 s. Reset on whether the attempt
  connected. On `release` too.
- `rs/moq-relay/src/config.rs`: `cluster.api.root` without `cluster.api.url`,
  and `cluster.token` on a relay with no peers to dial, are accepted and
  never read; refuse both at startup. On `release` too. A config key that
  starts failing is a break: note it in the PR's Impact for the migration
  audit.
- `rs/moq-auth/src/wire.rs` and `key.rs`: Rust accepts `null` on grant and key
  fields `@moq/auth` refuses (`"scope": null` loads as an unrestricted key).
  Use the existing reject-`null` deserializer and add corpus cases.
- Docs: `rs/moq-relay/MIGRATE.md` names `--mtls-peer`, which does not exist
  (`--mtls-link peer`), and never mentions that certificate-only peers now get
  client limits and are cut off past 1000 broadcasts; `doc/bin/relay/auth.md`
  says an older auth server reads new transport names as `unknown`, but a
  `release` `moq auth serve` answers 422.

Public API: none. Wire: none. Config: two ignored keys start failing.
