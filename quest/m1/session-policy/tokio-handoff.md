# [S] moq-tokio becomes a thin shell

## Goal

moq-tokio keeps only sockets, DNS, TLS, accept loops, the fallback runtime and
spawning, and calls moq-net for all session policy. It gains two public
handoffs for transports established elsewhere: `accept_with(transport, link)`
on the listener and `connect_with(transport)` on the client, which run the
moq-net accept or connect policy over an already-connected session and return
the usual request or session. moq-webrtc uses them.

## Plan

Part of [Session policy lives in moq-net](/quest/m1/session-policy/README.md);
see it for the decisions and research. This is what
[moq-dev/moq#7804](https://github.com/moq-dev/moq/pull/7804) and the
[WebRTC rename](/quest/m1/webrtc-rename.md) wait on: the maintainer decided on
2026-11-04 to move the policy first, so the handoff is a few lines over
moq-net rather than a copy of the policy.

- The handoff's signature (transport plus link metadata in, request or session
  out) is fixed by this quest and does not change if policy moves further.
- Delete moq-tokio's now-unused `Deadline`, `within`, `Backoff` and the like,
  and its per-transport `Request` construction that moq-net now owns.
- Its consumer is moq-webrtc, so the [API audit](/quest/m1/api-audit/README.md)
  rule (a new public surface needs a consumer) is met.

Public API: moq-tokio adds `accept_with` and `connect_with` and drops internals
it re-exported, with MIGRATE notes. Wire: none.

## Required

- [Accept policy in moq-net](/quest/m1/session-policy/accept.md) - the accept policy the handoff runs
- [Connect policy in moq-net](/quest/m1/session-policy/connect.md) - the connect policy the handoff runs
