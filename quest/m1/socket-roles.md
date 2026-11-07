# [L] Socket endpoints split into servers and clients

## Goal

A socket endpoint is a server, a client, or both, and the relay's ordinary
grants enforce which. A server announces `<prefix>/.server/<id>` and accepts
dials; a client announces `<prefix>/.client/<id>` and dials. A client's grant
can't announce as a server or read another client's endpoint, so roles come
from the grant rather than an id prefix such as boy's `viewer-*`. The
symmetric `.socket/<id>` layout is gone from moq-net (Rust, JS, Python through
moq-ffi), the [socket concept page](/doc/concept/socket.md), the interop
clients, moq-boy, and CARLA.

| Role | Publishes | Subscribes |
| --- | --- | --- |
| Server | `.server/<id>` | `.client/**` |
| Client | `.client/<id>` | `.server/**` |

Non-goals: stopping a client from announcing another client's id once it learns
it. Ids stay unguessable secrets until the m3 [mesh](/quest/m3/mesh/README.md)
makes them public keys; [CARLA's driver ids](/quest/m1/carla-driver-ids.md)
stops CARLA leaking them.

## Plan

Decided 2026-11-06 (maintainer, follow-up planning in `/quest-complete`, raised
while landing [moq-dev/moq#8011](https://github.com/moq-dev/moq/pull/8011) and
asking how anonymous viewers are kept from posing as cars or emulators):

- **What the split enforces?** The dialer still publishes: its half of a
  session, `P/A/B/S`, lives in its own broadcast, and the acceptor's
  subscription to it is the return-routability check.
  - ✅ Two hidden segments, one per role: a server publishes its own and reads
    clients', a client the reverse. Plain grants enforce it, with no relay
    change.
  - Relay-enforced roles on session track names (dial = `P/<self>/*/*`,
    listen = `P/*/<self>/*`).
  - Dial ids bound to the connection, assigned by the relay or held by the
    first session to claim them: hard across a cluster.
  - Rely on the mesh's public-key ids alone.
- **Names?** ✅ `.server` / `.client`, read as roles. Not chosen: `.accept` /
  `.dial` (the API verbs); `.listen` / `.dial`.
- **The old layout?** ✅ Replace `.socket/<id>`: one convention, and a peer
  that both dials and accepts announces both broadcasts under one id. Not
  chosen: keep `.socket` for symmetric peers beside the split.
- **Milestone?** ✅ m1, before sockets first publish from the cut. Not chosen:
  m3 beside the mesh's public-key ids; m4.

Lead, for the PR to confirm: an endpoint announces `.client/<id>` at its first
dial and `.server/<id>` at its first accept, so a pure client never announces a
server endpoint, and today's dial-reserves-its-half and
accept-from-the-first-accept rules carry over. The session track name, the
handshake, the return-routability check, and both protocols are unchanged; only
which broadcast holds each half moves (`P/A/B/S` in `.client/A` and in
`.server/B`).

Update with it:

- moq-boy (`rs/moq-boy`, `js/moq-boy`): the emulator is a server, viewers are
  clients; drop `VIEWER_ID_PREFIX` and the emulator's refusal of `viewer-`
  names.
- CARLA: trucks are servers, drivers clients; drop the `driver-` id prefix and
  the worker's refusal of `driver-*` trucks.
- `demo/relay/prod.toml` and its documented `moq auth serve` line: anonymous
  publish on `demo/boy/.client/*` and `demo/carla/.client/*`, subscribe on
  their `.server/**`.
- The [hosted demo deploy](/quest/m3/release/demo-sockets-deploy.md) uses
  these grants.

Public API: breaks `moq-net`'s `socket` module, `@moq/net`'s `socket` export,
and moq-ffi's socket bindings wherever they name the layout. Wire: none (a
path convention), but endpoints on the old layout can't reach new ones.
