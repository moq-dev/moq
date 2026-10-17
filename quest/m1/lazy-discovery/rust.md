# [L] Rust lazy discovery

## Goal

The Rust client session requests announcements only for prefixes its
`consume` origin is watching, matching the
[JS behavior](/doc/lib/js/net.md) and the
[line's shared rules](/quest/m1/lazy-discovery/README.md). Rust's `follow`
folds into its request path as in the
[JS read API](/doc/lib/js/net.md). Relays keep watching the
root, so relay behavior and cluster routing are unchanged.

## Plan

Decided 2026-10-10 in the [lazy discovery](/quest/m1/lazy-discovery/README.md)
interview: mirror JS, and settle the Rust API mapping in this quest's own
`/quest-plan` round before implementing.

Facts (2026-10-10, `origin/main` e5446b036):

- The lite subscriber builds one `AnnouncePrefix` per
  `interest_prefixes(&origin.allowed())` at session start
  (`rs/moq-net/src/lite/subscriber.rs:601`); the IETF subscriber has the
  equivalent.
- `OriginConsumer::follow` exists (`rs/moq-net/src/model/origin.rs:4473`);
  Rust has no announced-gated request like JS's, so the fold's shape is open.
- moq-relay reads `origin.consume().announced()` for the root in its web,
  internal, and cluster paths, so a relay's watch set is the root.

The JS shape to mirror (`js/net/src/connection/forward.ts`): the origin
counts watched literal heads in a trie and hands each session only the
covering-set changes a watch causes, so a change costs the prefixes it covers
or uncovers and stays flat over unrelated held watches
(`js/net/bench/forward.ts`); an announce request stays open while anything at
or beneath its prefix is watched, so a narrower watch rides it and never takes
its routes over (nothing tells when a fresh request has synced); a narrower
request already open when a broader watch arrives stays open until the broader
request goes; a route reported by overlapping requests is inserted once,
driven by the first reporter, and if it changes hands anyway (a request
failing) it restarts unless one epoch proves both saw one instance; closing an announce request ends its stream; a plain request
resolved through a discovered route watches its path until closed; a request
closes 2s after nothing beneath it is watched (a fixed constant, since one-shot
connections have no `linger`); a refused or reset request downgrades the
whole session to blind answers.

Open for the plan round: the Rust equivalent of an announced request, how the
watch set crosses the poll-based session driver, and moq-ffi and binding
fallout (see the Cross-Package Sync table in `AGENTS.md`).

Tests must pin the relay side: an origin in relay mode (cluster and upstream
sessions on the client subscriber path) still sends a root request, so
lazy discovery never narrows what a relay discovers.
