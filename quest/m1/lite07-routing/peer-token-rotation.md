# [M] A cluster peer's token rotates on its live session

## Goal

When a peer's token changes, either through `api.url` or by rotating
`cluster.token`, moq-relay renews authorization on the live session. Today
the first cancels the session with no GOAWAY or drain, so routes flap, and
the second takes effect only after the session fails. Any other change to a
peer's dial target still redials.

## Plan

Found 2026-11-21 by the main → release routing audit (maintainer picked it
as a quest, not gating the cut).

- Every peer credential is sent as `?jwt=` in the URL (`rs/moq-relay/src/cluster.rs:318`,
  `2231-2241`), and `peer_client` clears the template's tokens.
- A token change through `api.url` makes the dial target differ, so `upsert`
  removes the task and cancels the live session with no GOAWAY or drain
  (`cluster.rs:490-533`).
- A rotated `cluster.token` is used only when a redial follows a failure
  (`cluster.rs:2194-2203`), so the live session runs on until the old token
  expires.

Recommended: present a peer's token through the connection's live token
handle (`moq_net::auth::Tokens::set`) and leave the token out of the dial
target's identity, so a token-only change becomes an AUTH update on lite-07.
A session without the AUTH stream (lite-06, moq-transport) has to redial:
bring the new session up before draining the old one, so it never goes
dark. Test both with time mocked.

Public API: report any peer configuration change. Wire: none.

## Related

- [Tokens in URLs](/quest/m4/tokens-url-auth.md) - the opposite direction, URL credentials for clients
