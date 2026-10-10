# [M] IETF claim responses identify the served broadcast

## Goal

A relay learns the identity of a broadcast served under a prefix claim over
the IETF epoch extension, as it does over lite-07. A worker restarting one
output cannot splice its new groups into a cached old instance, and the
first viewer does not need a competing exact-path announcement.

## Plan

Decided 2026-10-10: claim-served epochs cover both protocols but remain a
separate m1 follow-up to announcement-based IETF epochs. The existing
claim-epochs quest owns the shared broadcast model and lite-07 wire; this
quest owns the IETF response identity and mixed-protocol verification. This
split keeps the lite-07 cut independent of the IETF extension.

Carry the accepted broadcast's identity in the negotiated response path and
pin later requests to it. Preserve the shared claim rules for adopting a
learned epoch, refusing another instance, and keeping existing readers
sticky. Do not copy a source epoch onto different derived content, mint
epochs in relays, or infer continuity from an epochless claim.

Mirror the claim-epochs regression scenarios over IETF and mixed lite-07 /
IETF chains: first viewer without a cut, restarted output with cached old
groups, stale learned identity refused, and a worker draining one output.
Include an epochless viewer on an updated relay that resubscribes after the
replacement. Use mocked time and existing CI lanes; run cross-language
interop. Update the extension draft and existing concept docs in the same
PR, with `just drafts check` and `just check`.

Public API: reuse the shared broadcast identity model. Wire: negotiated IETF
responses report the served identity; unnegotiated peers keep their wire.

## Required

- [IETF epochs](/quest/m1/ietf-epochs.md) - negotiation and announcement/request identity
- [Claim epochs](/quest/m1/claim-epochs.md) - the shared model and lite-07 reference behavior
