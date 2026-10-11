# [L] Negotiate broadcast epochs on IETF connections

## Goal

Rust and JS carry broadcast epochs over a negotiated moq-transport extension
on drafts 17 through 22 without changing the broadcast path. Matching
explicit epochs permit stitching; a different epoch or a different
epochless source is a new broadcast. Epochless clients remain supported on
updated relays.

## Plan

Decided 2026-10-10 in the epoch audit interview:

- This is m1, after the m0 identity correctness fixes. Do not make the
  extension or seamless JS handover a release blocker.
- Preserve the path. The previous `@<epoch>` suffix changed the name older
  clients subscribe to; rejected in favor of negotiated metadata.
- Use a standalone SETUP-negotiated extension on drafts 17-22, where the
  existing extension infrastructure applies, rather than draft 22 only.
  Drafts 14-16 and peers that do not negotiate it retain their existing wire.
  Integrate with the shared extension configuration in Rust and JS instead
  of inventing another configuration mechanism.
- Reuse lite-07's epoch representation and identity semantics. A relay
  preserves an explicit epoch, never invents continuity for an epochless
  source, and refuses malformed or mismatched identities.
- Carry identity through announcements and the requests that resolve,
  subscribe to, or fetch a broadcast. Pin a request to its chosen instance;
  a replacement must not substitute new content into an old request.
  Separate object and negative caches by identity, including joining FETCH
  and concurrent old/new subscriptions. Track aliases and response handling
  must preserve that separation too. This changes identity semantics, not
  merely advisory announcement metadata.
- Epoch and cluster extensions negotiate independently. The cluster draft
  already describes a shared negotiated NAMESPACE parameter block; make its
  framing unambiguous with either extension, both, or neither. Reconcile its
  namespace-only redundant-publisher semantics with explicit epoch opt-in.
- Support epochless clients on updated relays: preserve their paths and
  wire framing, translate replacements into withdrawal and reannouncement,
  and resolve fresh subscriptions to the current instance. Keep the relay's
  internal epoch and caches separated. A same-epoch upstream route handoff
  may remain invisible downstream. Existing subscriptions stay sticky;
  announcement-aware players reset and resubscribe on End+Start. A player
  that ignores these signals is not promised automatic recovery.
- Arbitrary older caching relays are out of scope. Base IETF object identity
  is namespace, track, group, and object ID, and withdrawal does not invalidate
  it. Do not claim that removing the epoch makes reused object names safe
  through those relays or through clients retaining caches across broadcasts.
  Immutable wire names plus discovery/translation would be separate work.
- Independent announcement takeover is a separate follow-up, sharing the
  lite-07 model. An epoch-bound request requires a matching advertised epoch;
  response metadata alone cannot make an epochless claim eligible. Do not
  block this base extension on takeover.

Add a feature-specific IETF extension draft in the implementation PR, with
negotiation, identity, cache, request, downgrade, and malformed-input rules.
Update the cluster draft and existing identity/compatibility concept docs
inline. The dedicated [identity guide](/quest/m1/identity-guide.md) follows
separately and does not replace those updates. The new draft must distinguish
our legacy-client behavior from base IETF's immutable-name guarantee; an
End+Start is not a general cache invalidation mechanism.

Verify Rust and JS with matching, different, and absent epochs; exact and
covering announcements; old subscriptions held across replacement; stale
SUBSCRIBE/FETCH refusals; and no stale catalog or media on new requests.
Exercise both directions across drafts 17-22, mixed lite-07/IETF relay
chains, and epochless clients. Cover independent epoch/cluster negotiation,
declining the extension on either side, and unchanged legacy framing.
Tests belong in existing CI lanes. Run `just check`, `just drafts check`,
and `just test interop --all`; benchmark any changed routing/cache fanout.

Public API: expose negotiation through the existing extension configuration
and reuse the epoch model in Rust and JS. Wire: an opt-in IETF identity
extension; no changes to unnegotiated published versions or broadcast paths.

## Required

- [Setup extensions](/quest/m1/auth/extensions.md) - the common configuration for offering extensions

## Related

- [Broadcast epochs](/quest/m0/broadcast-epoch/README.md) - current lite-07 identity and legacy restart behavior
- [Announcement takeover](/quest/m1/announce-takeover.md) - shared continuity model and lite-07 operation
- [IETF takeover](/quest/m1/ietf-takeover.md) - independently advertised continuation of a claim-served instance
- [JS track handover](/quest/m1/js-group-handover.md) - stable JS readers across same-epoch route changes
- [Catalog references](/quest/m0/broadcast-epoch/catalog-references.md) - optional requested epochs become verifiable on this negotiated transport; epochless fallback remains best effort
- [IETF fill timeout](/quest/m1/ietf-fill-timeout.md) - native wait-budget support is separate from epoch negotiation and adds no age extension
