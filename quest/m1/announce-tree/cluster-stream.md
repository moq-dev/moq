# [M] Cluster stream

## Goal

Authenticated lite-07 cluster peers can exchange the evidence and lifecycle
messages required by the validated pruning policy. Merely opening a stream or
receiving reachability information does not enable suppression. Older Lite,
IETF, and customer sessions retain today's announcement behavior.

## Plan

Specify the smallest encoding justified by the
[policy](/quest/m1/announce-tree/policy.md), including relay reachability and
any readiness, replacement, or invalidation messages it requires. Do not freeze
a parent/backup table that cannot represent its recovery contract.

- Carry application-authenticated cluster authorization into moq-net. A SETUP
  hop, certificate SNI, or protocol version alone does not authorize a peer to
  inject cluster state. Reject an unauthorized stream, including one from a
  customer that declared a hop.
- Bind evidence to session lifetimes and distinguish parallel sessions when
  the policy uses them. Define snapshot completion, stale generations,
  withdrawal, reconnect, and explicit return to flooding.
- Keep anonymous and unsupported source mappings outside pruning. Unknown
  optional capabilities fall back to flooding; malformed supported messages
  are protocol errors.
- Bound bytes, entry counts, and state; reuse existing hop/cost codecs where
  their semantics match. Count control bytes separately from announce bytes.
- Update drafts/draft-lcurley-moq-lite.md and Rust/JS protocol handling together.
  JS does not initiate the relay-only stream and rejects it when unauthorized.
  Report the exact wire and public API changes in the implementation PR.

Tests cover codec boundaries, authorization, old/IETF peers, parallel-session
identity, reordered control versus announcement delivery, disconnect cleanup,
and rebuilding state on reconnect. Negotiating lite-07 alone must not activate
pruning. Keep production forwarding unchanged until the forwarding quest.

## Required

- [Pruning policy](/quest/m1/announce-tree/policy.md) - validated state and transitions to encode

## Related

- [Hidden broadcasts](/quest/m1/hidden-broadcasts.md) - lite-07 discovery opt-in
- [Prefix table](/quest/m2/announce-prefix-table.md) - independent announce compression
