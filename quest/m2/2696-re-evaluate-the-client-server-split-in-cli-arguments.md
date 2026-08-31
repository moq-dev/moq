# [L] Re-evaluate the client/server split in CLI arguments

## Goal

Implement and verify the behavior tracked in [#2696](https://github.com/moq-dev/moq/issues/2696)
within the issue's stated scope and boundaries.

## Plan

Use the public issue's scope, implementation notes, and acceptance criteria
below as the starting plan. Reconcile paths and assumptions with the current
tree before implementation.

### Issue context

#### Problem

The CLI splits almost every transport knob into `--client-*` and `--server-*` variants, but a QUIC endpoint does both. The split is an artifact of `ClientConfig` and `ServerConfig` being separate Rust types, not something a user asked for, and it costs us a duplicated flag for every knob that isn't actually role-specific.

Eleven knobs currently exist in both spellings, with identical meaning:

| Duplicated | |
|---|---|
| `--client-bind` / `--server-bind` | the socket |
| `--client-backend` / `--server-backend` | quinn / quiche / noq |
| `--client-version` / `--server-version` | negotiated MoQ versions |
| `--client-tls-root` / `--server-tls-root` | trust roots |
| `--client-quic-*` / `--server-quic-*` | `congestion-control`, `gso`, `idle-timeout`, `keep-alive`, `max-streams`, `mtu-discovery`, `qlog` (7 pairs) |

Genuinely role-specific ones are the minority: dialing (`--client-connect`, `--client-connect-timeout`, `--client-failover-delay`, `--client-reconnect`, client cert/verification) and listening (`--server-tcp*`, `--server-unix*`, `--server-quic-lb-*`).

#### Why it's coming up now

`moq --cluster-lan` needs an endpoint that dials peers *and* accepts from them. With the current split there's no way to express that, so it reaches into `--server-bind`, fills it in with an ephemeral port and a generated certificate when unset, and separately clones the `ClientConfig` per peer to dial out. That works, but it means a user who typed one flag silently gets a wildcard listener they never asked for, and the mesh's dial settings and accept settings are configured through two unrelated prefixes.

That surfaced in review as a security concern. It isn't one (an unauthenticated `--cluster-lan` is the same exposure as a bare `--server-bind`, which is documented), but it is a real sign the flags no longer describe the thing being configured.

#### Direction

Roughly, in rough order of appetite:

- **`--bind` applies to both.** One endpoint, one socket flag. Keep a way to bind separate client/server sockets if anyone needs it, but stop making that the default shape.
- **`--connect` loses the `--client` prefix.** It's the URL to dial; nothing about it is client-role-specific in a way the prefix earns.
- **Collapse the shared knobs** (`--quic-*`, `--version`, `--backend`, `--tls-root`) into one spelling, with role-specific overrides only where a real asymmetry exists.
- **Keep prefixes only where the role genuinely differs**, e.g. accepting on a Unix socket, or presenting a client certificate.

#### `--cluster-*` is tangled too

Separate but related. There are now four ways to find a peer, and the names don't tell you they're alternatives:

- `--cluster-connect` (static peer list)
- `--cluster-connect-api` (dynamic peer list over HTTP/file)
- `--cluster-mesh` (gossip, needs `--cluster-node`)
- `--cluster-lan` (mDNS, needs `--cluster-node` and `--cluster-lan-secret`)

plus `--cluster-node` (this relay's own URL, meaning something different again), `--cluster-token`, `--cluster-id`, `--cluster-tier`, `--cluster-linger`. Several combinations are invalid and only fail at startup with a hand-written `ensure!`. Worth reshaping alongside the client/server work, since "how do I find peers" and "how do I bind an endpoint" are the same question from two directions.

#### Constraints

- Every rename is user-facing, so the old spellings stay as hidden aliases per the deprecation rules in `CLAUDE.md`: no `--help` entry, no "deprecated, use X" note, `/doc` examples updated to the new names only.
- The flags are also TOML keys, so a rename is a config-file migration, not just a CLI one.
- `moq-relay`, `moq-cli`, and `moq-bench` all flatten these configs; a change lands in all three at once.
- Sample invocations are scattered across `doc/bin/`, `doc/lib/`, `doc/setup/`, `doc/concept/`, and the `demo/` justfiles. `CLAUDE.md` already calls out grepping the binary name repo-wide and reconciling every hit against `--help`.

## Required

- [Plan: the CLI argument redesign](/quest/m2/plan-cli-arguments.md) - split into implementable quests first

## Closes

- [#2696](https://github.com/moq-dev/moq/issues/2696) - close this issue when the quest finishes
